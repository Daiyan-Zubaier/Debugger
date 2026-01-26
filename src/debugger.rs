use std::collections::HashMap;

use nix::Result;
use nix::libc::{SI_KERNEL, TRAP_BRKPT, TRAP_TRACE, c_long};
use nix::sys::ptrace::{self, AddressType};
use nix::sys::signal::Signal::{self, SIGCONT};
use nix::sys::wait::{WaitStatus, waitpid};
use nix::unistd::Pid;

use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;

use crate::breakpoint::Breakpoint;
use crate::elf_debug_info::ElfDebugInfo;
use crate::registers::{
  REG_DESCS, Register, get_register_from_name, get_register_value, set_register_value,
};

/// Struct to hold debugger information
pub struct Debugger {
  /// Path to program
  program_name: String,

  /// Program will run on child process, this is the pid for that
  pid: Pid,

  /// Flag to see if program is executing
  is_executing: bool,

  /// A hash map of all breakpoints
  breakpoints: HashMap<AddressType, Breakpoint>,

  /// AN object to hold debug info
  debug_info: ElfDebugInfo,
}

impl Debugger {
  /// Construct new Debugger object
  pub fn new(program_name: String, pid: Pid) -> Self {
    let debug_info = match ElfDebugInfo::new(program_name.clone()) {
      Ok(value) => value,
      Err(e) => panic!("Error constructing ElfDebugInfo: {}", e),
    };

    Self {
      program_name,
      pid,
      is_executing: false,
      breakpoints: HashMap::new(),
      debug_info,
    }
  }

  /// Run the debugger
  pub fn run(&mut self) -> rustyline::Result<()> {
    /*
     * For now Option is set to None. This means it only blocks until child exits or is killed.
     * options is a bitmask that determines which state transitions to block
     * Waits for thread to be ready
     */
    match waitpid(self.pid, None)? {
      // Sends SIGTRAP signal
      WaitStatus::Stopped(_, _) => {
        println!(
          "SIGTRAP received, {} ready to be debugged!",
          self.program_name
        );
      }
      _ => {
        println!("Unexpected status, returning.......");
        return Ok(());
      }
    }

    // Process is ready to be debugged, now let's start the command line input
    let mut rl = DefaultEditor::new()?;

    // Checks if file history feature is enabled
    if rl.load_history("history.txt").is_err() {
      println!("No prev history");
    }

    loop {
      // Wait for process state changes when executing
      if self.is_executing {
        match waitpid(self.pid, None)? {
          WaitStatus::Stopped(_, sig) => {
            println!("Stopped by {:?}", sig);

            match sig {
              Signal::SIGTRAP => self.handle_sigtrap()?,
              Signal::SIGSEGV => self.handle_sigsegv()?,
              _ => {
                println!("Got signal: {:?}", sig);
                self.is_executing = false;
              }
            }
          }
          WaitStatus::Exited(_, code) => {
            println!("Exited with {}", code);
            break;
          }
          WaitStatus::Signaled(_, sig, _) => {
            println!("Killed by {:?}", sig);
            break;
          }
          other => {
            println!("Other status: {:?}", other);
          }
        }
      }

      // Collect user input
      let readline = rl.readline("(rust_dbg) ");
      match readline {
        Ok(line) => {
          // Pass in command to command handler
          rl.add_history_entry(line.as_str())?;
          self.handle_command(&line)?;
        }
        Err(ReadlineError::Interrupted) => {
          println!("CTRL-C");
          break;
        }
        Err(ReadlineError::Eof) => {
          println!("CTRL-D");
          break;
        }
        Err(err) => {
          println!("Error: {:?}", err);
          break;
        }
      }
    }
    rl.save_history("history.txt")?;
    Ok(())
  }

  /// Command handler, pass in command to be "handled"
  ///
  /// Self NOte: More encapsulation is possible for a cleaner implementation
  ///
  /// For now the commands are:
  /// - continue (to continue execution of the program)
  /// - break (to set a breakpoint)
  /// - and more...
  fn handle_command(&mut self, line: &str) -> Result<()> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let command = match tokens.first() {
      Some(cmd) => *cmd,
      None => return Ok(()), /* empty line, do nothing */
    };

    match command {
      "continue" => {
        ptrace::cont(self.pid, SIGCONT)?;
        self.is_executing = true;
      }
      "break" => {
        let arg = match tokens.get(1) {
          Some(a) => *a,
          None => {
            println!("Usage: break 0x<addr>");
            return Ok(());
          }
        };

        let hex = match arg.strip_prefix("0x") {
          Some(h) => h,
          None => {
            println!("Usage: break 0x<addr> (hex)");
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

        // Heuristic: small addresses are usually DWARF/link-time addresses (e.g., 0x1139).
        // Large addresses are usually runtime (e.g., 0x5555...).
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
      }
      "next" | "n" => {
        self.step_over_line()?;
      }
      "register" => {
        match tokens[1] {
          "dump" => {
            for reg_desc in REG_DESCS.iter() {
              let value = get_register_value(self.pid, reg_desc.reg)?;
              println!("{:<10}  {:0>16x}", reg_desc.name, value);
            }
          }

          // Assumed command:
          // register read <reg_name>
          "read" => {
            let register_name = get_register_from_name(tokens[2]);
            match register_name {
              Some(reg_name) => println!("{}", get_register_value(self.pid, reg_name)?),
              None => println!("Invalid register_name...."),
            }
          }

          // Assumed command:
          // register write <reg_name> 0x<Value>
          "write" => {
            let value_string = tokens[3].strip_prefix("0x").unwrap_or(tokens[3]);
            let value = match u64::from_str_radix(value_string, 16) {
              Ok(v) => v,
              Err(_) => {
                println!("Invalid hex value: {}", value_string);
                return Ok(());
              }
            }; // let value 
            let reg_name = get_register_from_name(tokens[2]);
            match reg_name {
              Some(reg) => set_register_value(self.pid, reg, value)?,
              None => println!("Invalid register name..."),
            }
          }
          _ => {
            println!("Command not found");
          }
        }
      }
      "memory" => {
        let addr_val = usize::from_str_radix(tokens[2].strip_prefix("0x").unwrap(), 16).unwrap();
        let addr = addr_val as AddressType;
        match tokens[1] {
          "read" => {
            println!("{}", ptrace::read(self.pid, addr)?);
          }
          "write" => {
            let data = c_long::from_str_radix(tokens[2].strip_prefix("0x").unwrap(), 16).unwrap();
            ptrace::write(self.pid, addr, data)?;
            println!("Data {} was written to address {}", data, addr_val);
          }
          _ => {
            println!("Invalid memory command");
          }
        }
      }
      _ => {
        println!("Invalid command");
      }
    }
    Ok(())
  }

  /// Step over breakpoint
  fn stepover_breakpoint(&mut self) -> Result<()> {
    let possible_bp_location = (self.get_pc()? - 1) as AddressType;

    // First check if breakpoint exists and disable it
    if let Some(bp) = self.breakpoints.get_mut(&possible_bp_location) {
      if bp.enabled_status {
        bp.disable()?;
      } else {
        return Ok(()); // Not enabled, nothing to do
      }
    } else {
      return Ok(()); // No breakpoint at this location
    }
    // Mutable borrow of self.breakpoints ends here when bp goes out of scope

    let prev_instruction_address = possible_bp_location as u64;
    self.set_pc(prev_instruction_address)?;
    ptrace::step(self.pid, None)?;
    waitpid(self.pid, None)?;

    // Re-borrow mutably to re-enable the breakpoint
    if let Some(bp) = self.breakpoints.get_mut(&possible_bp_location) {
      bp.enable()?;
    }

    Ok(())
  }

  /// Get program counter
  fn get_pc(&self) -> Result<u64> {
    get_register_value(self.pid, Register::Rip)
  }

  /// Set program counter
  fn set_pc(&self, value: u64) -> Result<()> {
    set_register_value(self.pid, Register::Rip, value)
  }

  /// Prints location
  fn print_location(&self, runtime_pc: u64) {
    let dwarf_pc = self.to_dwarf_pc(runtime_pc);

    let func = self.debug_info.pc_to_function(dwarf_pc).ok().flatten();
    let loc = self.debug_info.pc_to_file_line(dwarf_pc).ok().flatten();

    match (func, loc) {
      (Some(f), Some((file, line))) => {
        println!("At {f} ({file}:{line})");
      }
      (Some(f), None) => {
        println!("At {f} (no line info)");
      }
      (None, Some((file, line))) => {
        println!("At {file}:{line}");
      }
      (None, None) => {
        println!("At 0x{:x} (no DWARF match)", runtime_pc);
      }
    }
  }

  /// Loads bias
  fn load_bias(&self) -> std::io::Result<u64> {
    let maps_path = format!("/proc/{}/maps", self.pid);
    let maps = std::fs::read_to_string(maps_path)?;

    let prog_base = self
      .program_name
      .rsplit('/')
      .next()
      .unwrap_or(&self.program_name);

    for line in maps.lines() {
      // Example:
      // 555555554000-555555556000 r-xp 00000000 08:01 12345 /path/to/program_target/main
      // 555555556000-555555557000 r-xp 00001000 08:01 12345 /path/to/program_target/main
      let mut it = line.split_whitespace();

      let range = match it.next() {
        Some(v) => v,
        None => continue,
      };
      let perms = match it.next() {
        Some(v) => v,
        None => continue,
      };
      let offset_hex = match it.next() {
        Some(v) => v,
        None => continue,
      };
      let _dev = it.next();
      let _inode = it.next();
      let path = it.next(); // may be None for anon maps

      if perms != "r-xp" {
        continue;
      }
      let Some(path) = path else { continue };

      if !(path.ends_with(&self.program_name) || path.ends_with(prog_base)) {
        continue;
      }

      let start_hex = range.split('-').next().unwrap();
      let start = u64::from_str_radix(start_hex, 16)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

      let offset = u64::from_str_radix(offset_hex, 16)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

      // Key correction:
      // bias is mapping start minus file offset.
      return Ok(start.saturating_sub(offset));
    }

    Err(std::io::Error::new(
      std::io::ErrorKind::NotFound,
      "could not determine load bias from /proc/<pid>/maps",
    ))
  }

  fn to_dwarf_pc(&self, runtime_pc: u64) -> u64 {
    match self.load_bias() {
      Ok(bias) => runtime_pc.saturating_sub(bias),
      Err(_) => runtime_pc, // fallback: non-PIE or couldn't read maps
    }
  }

  fn to_runtime_addr(&self, dwarf_addr: u64) -> u64 {
    match self.load_bias() {
      Ok(bias) => bias.saturating_add(dwarf_addr),
      Err(_) => dwarf_addr,
    }
  }

  fn step_over_line(&mut self) -> Result<()> {
    let start_line = self.get_current_line();

    loop {
      // Single step one instruction
      ptrace::step(self.pid, None)?;

      match waitpid(self.pid, None)? {
        WaitStatus::Stopped(_, sig) => {
          if sig != nix::sys::signal::Signal::SIGTRAP {
            println!("Stopped by {:?}", sig);
            return Ok(());
          }
        }
        WaitStatus::Exited(_, code) => {
          println!("Exited with {}", code);
          return Ok(());
        }
        _ => {}
      }

      let current_pc = self.get_pc()?;
      let current_line = self.get_current_line();

      // Stop if we're on a different line (or couldn't determine lines)
      match (&start_line, &current_line) {
        (Some((start_file, start_ln)), Some((cur_file, cur_ln))) => {
          if start_file != cur_file || start_ln != cur_ln {
            self.print_location(current_pc);
            break;
          }
        }
        _ => {
          // No debug info available, just do one step
          self.print_location(current_pc);
          break;
        }
      }
    }
    Ok(())
  }

  /// Get current file and line
  fn get_current_line(&self) -> Option<(String, u64)> {
    let pc = self.get_pc().ok()?;
    let dwarf_pc = self.to_dwarf_pc(pc);
    self.debug_info.pc_to_file_line(dwarf_pc).ok().flatten()
  }

  fn handle_sigtrap(&mut self) -> Result<()> {
    let sig_info = ptrace::getsiginfo(self.pid)?;

    match sig_info.si_code {
      // Check if breakpoint
      x if x == TRAP_BRKPT || x == SI_KERNEL => {
        let mut pc = self.get_pc()?;
        let bp_address = pc.saturating_sub(1) as AddressType;

        let hit_bp = self.breakpoints.contains_key(&bp_address);
        if hit_bp {
          pc = pc.saturating_sub(1);
        }

        println!("PC = 0x{:x}", pc);
        self.print_location(pc);

        if hit_bp {
          self.stepover_breakpoint()?;
        }

        self.is_executing = false;
      }
      TRAP_TRACE => {
        println!("UNIMPLEMENTED");
      }
      _ => println!("Uknown SIGTRAP code: {}", sig_info.si_code),
    };

    Ok(())
  }

  fn handle_sigsegv(&mut self) -> Result<()> {
    let sig_info = ptrace::getsiginfo(self.pid)?;

    println!(
      "Segfault! Reason Code: {}, Address {:?}",
      sig_info.si_code,
      unsafe { sig_info.si_addr() }
    );

    self.is_executing = false;
    
    Ok(())
  }
}
