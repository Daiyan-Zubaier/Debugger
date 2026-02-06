use nix::Result;
use nix::libc::c_long;
use nix::sys::ptrace::{self, AddressType};
use nix::sys::wait::waitpid;

use crate::breakpoint::Breakpoint;
use crate::debugger::Debugger;
use crate::registers::{
  REG_DESCS, Register, get_register_from_name, get_register_value, set_register_value,
};

// Command handling for the debugger
//
// This module contains methods for processing user commands.

impl Debugger {
  /// Command handler, pass in command to be "handled"
  ///
  /// For now the commands are:
  /// - continue (to continue execution of the program)
  /// - break (to set a breakpoint)
  /// - and more...
  pub(crate) fn handle_command(&mut self, line: &str) -> Result<()> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let command = match tokens.first() {
      Some(cmd) => *cmd,
      None => return Ok(()), /* empty line, do nothing */
    };

    match command {
      "continue" | "c" => self.cmd_continue()?,
      "break" => self.cmd_break(&tokens)?,
      "next" | "n" => self.step_over()?,
      "register" => self.cmd_register(&tokens)?,
      "memory" => self.cmd_memory(&tokens)?,
      "stepi" => self.single_step()?,
      "finish" => self.step_out()?,
      "step" | "s" => self.step_in()?,
      _ => println!("Invalid command"),
    }
    Ok(())
  }

  /// Handle continue command
  fn cmd_continue(&mut self) -> Result<()> {
    if self.has_crashed {
      println!("Warning: Program has crashed. Continuing will terminate the process.");
    }

    // Clean up any temp breakpoints from step_over commands
    let temp_addrs: Vec<_> = self.temp_breakpoints.iter().copied().collect();
    for addr in temp_addrs {
      self.cleanup_temp_breakpoint(addr)?;
    }

    // Note: stepover_breakpoint is already called by signal handler when we hit breakpoint
    ptrace::cont(self.pid, None)?;
    self.is_executing = true;
    Ok(())
  }

  /// Handle break command
  /// Supports: break 0x<addr> OR break filename:line
  fn cmd_break(&mut self, tokens: &[&str]) -> Result<()> {
    let arg = match tokens.get(1) {
      Some(a) => *a,
      None => {
        println!("Usage: break 0x<addr>  OR  break <filename>:<line>");
        return Ok(());
      }
    };

    // Check if it's a source-level breakpoint (contains ':' but doesn't start with '0x')
    if !arg.starts_with("0x") && arg.contains(':') {
      return self.cmd_break_source(arg);
    }

    // Address breakpoint
    let hex = match arg.strip_prefix("0x") {
      Some(h) => h,
      None => {
        println!("Usage: break 0x<addr>  OR  break <filename>:<line>");
        return Ok(());
      }
    };

    let addr_in = match u64::from_str_radix(hex, 16) {
      Ok(v) => v,
      Err(_) => {
        println!("Invalid address: {arg}");
        return Ok(());
      }
    };

    // Heuristic: small addresses are usually DWARF/link-time addresses (ex, 0x1139)
    // Large addresses are usually runtime (ex, 0x5555...)
    let runtime_addr = if addr_in < 0x400000 {
      self.to_runtime_addr(addr_in)
    } else {
      addr_in
    };

    let bp_addr = runtime_addr as AddressType;

    let mut bp = Breakpoint::new(self.pid, bp_addr);
    match bp.enable() {
      Ok(_) => println!("Breakpoint set at 0x{:x}", runtime_addr),
      Err(e) => println!("Failed to set breakpoint: {:?}", e),
    }

    self.breakpoints.insert(bp_addr, bp);
    Ok(())
  }

  /// Handle source-level breakpoint: break filename:line
  fn cmd_break_source(&mut self, arg: &str) -> Result<()> {
    // Parse filename:line
    let parts: Vec<&str> = arg.rsplitn(2, ':').collect();
    if parts.len() != 2 {
      println!("Invalid format. Use: break <filename>:<line>");
      return Ok(());
    }

    let line_str = parts[0];
    let filename = parts[1];

    let line_num: u64 = match line_str.parse() {
      Ok(n) => n,
      Err(_) => {
        println!("Invalid line number: {}", line_str);
        return Ok(());
      }
    };

    // Look up addresses for this file:line
    let addresses = match self.debug_info.file_line_to_addr(filename, line_num) {
      Ok(addrs) => addrs,
      Err(e) => {
        println!("Error looking up location: {:?}", e);
        return Ok(());
      }
    };

    if addresses.is_empty() {
      println!("No code found at {}:{}", filename, line_num);
      return Ok(());
    }

    // Set breakpoint at the first address (DWARF address, need to convert to runtime)
    let dwarf_addr = addresses[0];
    let runtime_addr = self.to_runtime_addr(dwarf_addr);
    let bp_addr = runtime_addr as AddressType;

    if self.breakpoints.contains_key(&bp_addr) {
      println!(
        "Breakpoint already exists at {}:{} (0x{:x})",
        filename, line_num, runtime_addr
      );
      return Ok(());
    }

    let mut bp = Breakpoint::new(self.pid, bp_addr);
    match bp.enable() {
      Ok(_) => println!(
        "Breakpoint set at {}:{} (0x{:x})",
        filename, line_num, runtime_addr
      ),
      Err(e) => println!("Failed to set breakpoint: {:?}", e),
    }

    self.breakpoints.insert(bp_addr, bp);
    Ok(())
  }

  /// Handle register command
  fn cmd_register(&mut self, tokens: &[&str]) -> Result<()> {
    let subcmd = match tokens.get(1) {
      Some(s) => *s,
      None => {
        println!("Usage: register <dump|read|write> ...");
        return Ok(());
      }
    };

    match subcmd {
      "dump" => {
        for reg_desc in REG_DESCS.iter() {
          let value = get_register_value(self.pid, reg_desc.reg)?;
          println!("{:<10}  {:0>16x}", reg_desc.name, value);
        }
      }

      // Assumed command:
      // register read <reg_name>
      "read" => {
        let reg_name_str = match tokens.get(2) {
          Some(s) => *s,
          None => {
            println!("Usage: register read <reg_name>");
            return Ok(());
          }
        };
        let register_name = get_register_from_name(reg_name_str);
        match register_name {
          Some(reg_name) => println!("{}", get_register_value(self.pid, reg_name)?),
          None => println!("Invalid register_name...."),
        }
      }

      // Assumed command:
      // register write <reg_name> 0x<Value>
      "write" => {
        let reg_name_str = match tokens.get(2) {
          Some(s) => *s,
          None => {
            println!("Usage: register write <reg_name> 0x<value>");
            return Ok(());
          }
        };
        let value_str = match tokens.get(3) {
          Some(s) => *s,
          None => {
            println!("Usage: register write <reg_name> 0x<value>");
            return Ok(());
          }
        };

        let value_string = value_str.strip_prefix("0x").unwrap_or(value_str);
        let value = match u64::from_str_radix(value_string, 16) {
          Ok(v) => v,
          Err(_) => {
            println!("Invalid hex value: {}", value_string);
            return Ok(());
          }
        };
        let reg_name = get_register_from_name(reg_name_str);
        match reg_name {
          Some(reg) => set_register_value(self.pid, reg, value)?,
          None => println!("Invalid register name..."),
        }
      }
      _ => {
        println!("Command not found");
      }
    }
    Ok(())
  }

  /// Handle memory command
  fn cmd_memory(&mut self, tokens: &[&str]) -> Result<()> {
    let subcmd = match tokens.get(1) {
      Some(s) => *s,
      None => {
        println!("Usage: memory <read|write> 0x<addr> [value]");
        return Ok(());
      }
    };

    let addr_str = match tokens.get(2) {
      Some(s) => *s,
      None => {
        println!("Usage: memory <read|write> 0x<addr>");
        return Ok(());
      }
    };

    let addr_val = match addr_str.strip_prefix("0x") {
      Some(hex) => match usize::from_str_radix(hex, 16) {
        Ok(v) => v,
        Err(_) => {
          println!("Invalid address: {}", addr_str);
          return Ok(());
        }
      },
      None => {
        println!("Address must be in hex format: 0x<addr>");
        return Ok(());
      }
    };

    let addr = addr_val as AddressType;

    match subcmd {
      "read" => {
        println!("{}", ptrace::read(self.pid, addr)?);
      }
      "write" => {
        let data_str = match tokens.get(3) {
          Some(s) => *s,
          None => {
            println!("Usage: memory write 0x<addr> 0x<value>");
            return Ok(());
          }
        };
        let data = match data_str.strip_prefix("0x") {
          Some(hex) => match c_long::from_str_radix(hex, 16) {
            Ok(v) => v,
            Err(_) => {
              println!("Invalid data value: {}", data_str);
              return Ok(());
            }
          },
          None => {
            println!("Data must be in hex format: 0x<value>");
            return Ok(());
          }
        };
        ptrace::write(self.pid, addr, data)?;
        println!("Data {} was written to address {}", data, addr_val);
      }
      _ => {
        println!("Invalid memory command");
      }
    }
    Ok(())
  }

  /// Step into: single-step instructions until the source line changes,
  /// then print the new source line
  pub(crate) fn step_in(&mut self) -> Result<()> {
    if self.has_crashed {
      println!(
        "Cannot step: program has crashed. Use 'continue' to terminate or restart the debugger."
      );
      return Ok(());
    }

    let start_line = self.get_current_line();

    // If we're not in user code and have breakpoints, suggest continue
    if start_line.is_none() && !self.breakpoints.is_empty() {
      println!("Not in user code (likely in dynamic linker or library).");
      println!("Use 'continue' to run to your breakpoint first.");
      return Ok(());
    }

    // If we don't have line info, step until we find code with debug info
    // (ex, returning from a library call) or hit a limit
    if start_line.is_none() {
      let mut steps = 0usize;
      loop {
        self.single_step()?;
        waitpid(self.pid, None)?;
        steps += 1;

        // Check if we now have line info (back in user code)
        if self.get_current_line().is_some() {
          break;
        }

        if steps > 10000 {
          let pc = self.get_pc()?;
          println!(
            "Stepped {} instructions, still in code without debug info at 0x{:x}",
            steps, pc
          );
          println!(
            "Use 'finish' to return from current function or 'continue' to run to next breakpoint"
          );
          return Ok(());
        }
      }

      let pc = self.get_pc()?;
      self.print_location(pc);
      if let Some((file, line)) = self.get_current_line() {
        let _ = self.print_source(&file, line);
      }
      return Ok(());
    }

    let start_line_num = start_line.as_ref().map(|(_, l)| *l);

    // Cap iterations to avoid pathological cases where line info doesn't advance
    let mut steps = 0usize;
    loop {
      self.single_step()?;
      waitpid(self.pid, None)?;

      steps += 1;

      let current_line = self.get_current_line().map(|(_, l)| l);
      if current_line != start_line_num {
        break;
      }

      if steps > 4096 {
        // Fallback: stop stepping to avoid infinite loop
        println!("Step limit reached");
        break;
      }
    }

    let pc = self.get_pc()?;
    self.print_location(pc);
    if let Some((file, line)) = self.get_current_line() {
      let _ = self.print_source(&file, line);
    }

    Ok(())
  }

  /// Step out: set breakpoint at return address and continue until hit
  /// The temporary breakpoint is cleaned up by the signal handler
  pub(crate) fn step_out(&mut self) -> Result<()> {
    if self.has_crashed {
      println!(
        "Cannot finish: program has crashed. Use 'continue' to terminate or restart the debugger."
      );
      return Ok(());
    }

    // Clean up any temp breakpoints from previous step_over calls
    let temp_addrs: Vec<_> = self.temp_breakpoints.iter().copied().collect();
    for addr in temp_addrs {
      self.cleanup_temp_breakpoint(addr)?;
    }

    // Try multiple ways to find return address depending on where we are in the prologue
    let frame_ptr = get_register_value(self.pid, Register::Rbp)?;
    let stack_ptr = get_register_value(self.pid, Register::Rsp)?;

    let mut return_address = None;

    // Collect all candidate return addresses and pick the best one
    let mut candidates: Vec<u64> = Vec::new();

    // Method 1: [RSP] - works at function entry (before push rbp)
    if let Ok(rsp_value) = ptrace::read(self.pid, stack_ptr as AddressType) {
      let rsp_value = rsp_value as u64;
      if self.is_any_executable_address(rsp_value) {
        candidates.push(rsp_value);
      }
    }

    // Method 2: [RSP+8] - works right after push rbp, before mov rsp, rbp
    if let Ok(rsp_plus_8) = ptrace::read(self.pid, (stack_ptr + 8) as AddressType) {
      let rsp_plus_8 = rsp_plus_8 as u64;
      if self.is_any_executable_address(rsp_plus_8) && !candidates.contains(&rsp_plus_8) {
        candidates.push(rsp_plus_8);
      }
    }

    // Method 3: [RBP+8] - traditional method after prologue completes
    if let Ok(rbp_plus_8) = ptrace::read(self.pid, (frame_ptr + 8) as AddressType) {
      let rbp_plus_8 = rbp_plus_8 as u64;
      if self.is_any_executable_address(rbp_plus_8) && !candidates.contains(&rbp_plus_8) {
        candidates.push(rbp_plus_8);
      }
    }

    // Prefer return addresses in OUR program over library addresses
    // This handles the case where RBP+8 points to libc but RSP points to our code
    for &addr in &candidates {
      if self.is_valid_code_address(addr) {
        return_address = Some(addr);
        break;
      }
    }

    // If none are in our program, use the first valid candidate (library address)
    if return_address.is_none() && !candidates.is_empty() {
      return_address = Some(candidates[0]);
    }

    // If we found a valid return address, set breakpoint there
    if let Some(ret_addr) = return_address {
      self.set_temp_breakpoint(ret_addr as AddressType)?;
      // Note: stepover_breakpoint is already called by signal handler when we hit breakpoint
      ptrace::cont(self.pid, None)?;
      self.is_executing = true;
      return Ok(());
    }

    // Fallback to single-stepping
    println!("Cannot determine valid return address.");
    println!("Falling back to single-stepping until function returns.");
    self.step_out_by_stepping()
  }

  /// Fallback step_out: single-step until we exit the current function
  fn step_out_by_stepping(&mut self) -> Result<()> {
    let start_func = self
      .debug_info
      .pc_to_function(self.get_dwarf_pc()?)
      .ok()
      .flatten();

    // Get starting frame pointer - when RBP increases past this, we've returned
    let start_rbp = get_register_value(self.pid, Register::Rbp)?;

    let mut steps = 0usize;

    loop {
      self.single_step()?;

      // Check if process exited
      match waitpid(self.pid, None)? {
        nix::sys::wait::WaitStatus::Exited(_, code) => {
          println!("Process exited with code {}", code);
          return Ok(());
        }
        nix::sys::wait::WaitStatus::Signaled(_, sig, _) => {
          println!("Process killed by signal {:?}", sig);
          return Ok(());
        }
        _ => {} // Continue stepping
      }

      steps += 1;

      let pc = self.get_pc()?;
      let current_rbp = get_register_value(self.pid, Register::Rbp)?;

      // Check if we ended up at an invalid address (stack, etc.)
      if !self.is_valid_code_address(pc) {
        println!("Warning: Stepped to non-code address 0x{:x}", pc);
        println!("The program may have crashed or stack may be corrupted.");
        self.has_crashed = true;
        return Ok(());
      }

      // Check if we're now in a different function
      let current_func = self
        .debug_info
        .pc_to_function(self.get_dwarf_pc()?)
        .ok()
        .flatten();

      // If RBP has increased past our starting frame, we've returned from the function
      // (Stack grows down, so a larger RBP means fewer frames on stack)
      if current_rbp > start_rbp {
        // Check if we're in user code (another function) or library code
        if current_func.is_some() {
          // Back in user code, in a different function - we've returned
          break;
        } else {
          // In library code (like libc's exit code after main returns)
          println!("Function returned. Now in library code at 0x{:x}.", pc);
          println!("Use 'continue' to run to next breakpoint or program exit.");
          return Ok(());
        }
      }

      // Also check if function changed while at same or deeper stack level
      if current_func.is_some() && current_func != start_func {
        break;
      }

      if steps > 100000 {
        println!("Step limit reached while trying to exit function");
        break;
      }
    }

    let pc = self.get_pc()?;
    println!("PC = 0x{:x}", pc);
    self.print_location(pc);
    if let Some((file, line)) = self.get_current_line() {
      let _ = self.print_source(&file, line);
    }

    Ok(())
  }

  /// Step over: execute current line, stepping OVER function calls
  /// Sets breakpoints at all lines in current function + return address, then continues
  pub(crate) fn step_over(&mut self) -> Result<()> {
    if self.has_crashed {
      println!(
        "Cannot step: program has crashed. Use 'continue' to terminate or restart the debugger."
      );
      return Ok(());
    }

    // Check if we're in user code first
    let start_line = self.get_current_line();
    if start_line.is_none() && !self.breakpoints.is_empty() {
      println!("Not in user code (likely in dynamic linker or library).");
      println!("Use 'continue' to run to your breakpoint first.");
      return Ok(());
    }

    let dwarf_pc = self.get_dwarf_pc()?;

    // Get the function's address ranges (may be non-contiguous in optimized code)
    let Some(func_ranges) = self.debug_info.get_function_ranges(dwarf_pc).ok().flatten() else {
      println!("Cannot determine function boundaries - falling back to step_in");
      return self.step_in();
    };

    // Get all line addresses in this function's ranges (DWARF addresses)
    let line_addrs = self
      .debug_info
      .get_line_addresses_in_ranges(&func_ranges, dwarf_pc)
      .unwrap_or_default();

    // Count addresses AFTER our current position
    let forward_addrs: Vec<_> = line_addrs
      .iter()
      .filter(|&&a| a > dwarf_pc)
      .copied()
      .collect();

    // If we're at or past all line addresses in the function, we're at the "last line"
    // In this case, step_over should behave like step_out - exit the function
    if forward_addrs.is_empty() {
      return self.step_out_by_stepping();
    }

    // Set temporary breakpoints only at addresses AFTER our current position
    for dwarf_addr in forward_addrs {
      let runtime_addr = self.to_runtime_addr(dwarf_addr) as AddressType;

      if !self.breakpoints.contains_key(&runtime_addr) {
        self.set_temp_breakpoint(runtime_addr)?;
      }
    }

    // Try to set breakpoint at return address (in case we're stepping over a call)
    // Try multiple approaches since frame pointers aren't always reliable
    let frame_ptr = get_register_value(self.pid, Register::Rbp)?;
    let stack_ptr = get_register_value(self.pid, Register::Rsp)?;

    // Try various ways to find return address
    let mut return_bp_set = false;

    // Method 1: Traditional RBP+8 (may fail if frame not set up yet)
    // Use is_any_executable_address since return may be to library code
    if let Ok(rbp_return) = ptrace::read(self.pid, (frame_ptr + 8) as AddressType) {
      let rbp_return = rbp_return as u64;
      if self.is_any_executable_address(rbp_return) {
        let addr = rbp_return as AddressType;
        if !self.breakpoints.contains_key(&addr) {
          self.set_temp_breakpoint(addr)?;
          return_bp_set = true;
        }
      }
    }

    // Method 2: Try RSP (for leaf functions or after prologue)
    if !return_bp_set {
      if let Ok(rsp_return) = ptrace::read(self.pid, stack_ptr as AddressType) {
        let rsp_return = rsp_return as u64;
        if self.is_any_executable_address(rsp_return) {
          let addr = rsp_return as AddressType;
          if !self.breakpoints.contains_key(&addr) {
            self.set_temp_breakpoint(addr)?;
          }
        }
      }
    }

    // Continue execution
    // Note: stepover_breakpoint is already called by signal handler when we hit breakpoint
    ptrace::cont(self.pid, None)?;
    self.is_executing = true;

    Ok(())
  }

  /// Set a temporary breakpoint (will be removed after being hit)
  fn set_temp_breakpoint(&mut self, addr: AddressType) -> Result<()> {
    if self.breakpoints.contains_key(&addr) {
      return Ok(()); // Already have a breakpoint here
    }

    // Validate that address is in ANY executable code region (including libraries)
    // This is needed for return addresses that may point to libc
    if !self.is_any_executable_address(addr as u64) {
      return Ok(());
    }

    let mut bp = Breakpoint::new(self.pid, addr);
    bp.enable()?;
    self.breakpoints.insert(addr, bp);
    self.temp_breakpoints.insert(addr);
    Ok(())
  }

  /// Clean up a single temporary breakpoint
  pub(crate) fn cleanup_temp_breakpoint(&mut self, addr: AddressType) -> Result<()> {
    if self.temp_breakpoints.remove(&addr) {
      if let Some(bp) = self.breakpoints.get_mut(&addr) {
        if bp.enabled_status {
          bp.disable()?;
        }
      }
      self.breakpoints.remove(&addr);
    }
    Ok(())
  }

  /// Clean up all remaining temporary breakpoints
  pub(crate) fn cleanup_all_temp_breakpoints(&mut self) -> Result<()> {
    let addrs: Vec<_> = self.temp_breakpoints.iter().copied().collect();
    for addr in addrs {
      self.cleanup_temp_breakpoint(addr)?;
    }
    Ok(())
  }

  /// Get program counter as DWARF address (offset by load bias)
  fn get_dwarf_pc(&self) -> Result<u64> {
    let pc = self.get_pc()?;
    Ok(self.to_dwarf_pc(pc))
  }
}
