use anyhow::{Context, Result};
use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;

use crate::target::DebugTarget;

use super::debugger::{RemoteArmDebugger, normalize_thumb_addr};
use super::gdb_protocol::{
  bytes_to_hex, expect_ok, format_stop_reply, hex_to_bytes, is_console_output_packet,
  parse_hex_u64, stop_reply_is_exit, u32_to_le_bytes,
};
use super::registers::arm_register_from_name;

const SOURCE_STEP_LIMIT: usize = 4096;

impl RemoteArmDebugger {
  /// Start the ARM REPL after negotiating with the remote target
  pub fn run(&mut self) -> Result<()> {
    self.initialize_remote()?;

    let mut rl = DefaultEditor::new()?;
    if rl.load_history("history.txt").is_err() {
      println!("No prev history");
    }

    loop {
      let readline = rl.readline("(rust_dbg arm) ");
      match readline {
        Ok(line) => {
          let _ = rl.add_history_entry(line.as_str())?;
          if !self.handle_command(&line)? {
            break;
          }
        }
        Err(ReadlineError::Interrupted) => {
          self.halt_target()?;
        }
        Err(ReadlineError::Eof) => break,
        Err(err) => {
          println!("Error: {:?}", err);
          break;
        }
      }
    }

    rl.save_history("history.txt")?;
    Ok(())
  }

  /// Print initial remote state before accepting commands
  fn initialize_remote(&mut self) -> Result<()> {
    println!(
      "Connected to ARM GDB remote target at {} for {}",
      self.endpoint, self.program_name
    );

    let (supported, stop_reason) = self.remote_initial_stop()?;
    if !supported.is_empty() {
      println!("Remote features: {}", supported);
    }
    println!("{}", stop_reason.message());
    self.print_current_location()?;
    Ok(())
  }

  /// Dispatch one ARM REPL command
  fn handle_command(&mut self, line: &str) -> Result<bool> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let command = match tokens.first() {
      Some(cmd) => *cmd,
      None => return Ok(true),
    };

    match command {
      "continue" | "c" => self.cmd_continue()?,
      "stepi" | "si" => self.cmd_stepi()?,
      "step" | "s" => self.step_source()?,
      "next" | "n" => self.next_source()?,
      "break" | "b" => self.cmd_break(&tokens)?,
      "delete" | "clear" => self.cmd_clear_breakpoint(&tokens)?,
      "register" => self.cmd_register(&tokens)?,
      "memory" => self.cmd_memory(&tokens)?,
      "backtrace" | "bt" => self.print_backtrace()?,
      "halt" => self.halt_target()?,
      "reset" => {
        self.monitor_command("reset halt")?;
        self.print_current_location()?;
      }
      "monitor" => self.cmd_monitor(&tokens)?,
      "help" | "h" => print_arm_help(),
      "quit" | "q" => return Ok(false),
      _ => println!("Invalid command. Try 'help'."),
    }

    Ok(true)
  }

  /// Continue the remote target until it stops
  fn cmd_continue(&mut self) -> Result<()> {
    let reply = self.conn.cont()?;
    println!("{}", format_stop_reply(&reply));
    self.print_current_location()
  }

  /// Single-step one remote target instruction
  fn cmd_stepi(&mut self) -> Result<()> {
    let reply = self.conn.step()?;
    println!("{}", format_stop_reply(&reply));
    self.print_current_location()
  }

  /// Step instructions until the source line changes
  fn step_source(&mut self) -> Result<()> {
    let start_line = self.get_current_line()?;

    for _ in 0..SOURCE_STEP_LIMIT {
      let reply = self.conn.step()?;
      if stop_reply_is_exit(&reply) {
        println!("{}", format_stop_reply(&reply));
        return Ok(());
      }

      let current_line = self.get_current_line()?;
      if current_line != start_line {
        println!("{}", format_stop_reply(&reply));
        self.print_current_location()?;
        return Ok(());
      }
    }

    println!("Step limit reached");
    self.print_current_location()
  }

  /// Execute the current source line, stepping over calls where possible
  fn next_source(&mut self) -> Result<()> {
    let reason = <Self as DebugTarget>::next_source(self)?;
    println!("{}", reason.message());
    self.print_current_location()
  }

  /// Set an ARM breakpoint from address or source-location tokens
  fn cmd_break(&mut self, tokens: &[&str]) -> Result<()> {
    let arg = match tokens.get(1) {
      Some(arg) => *arg,
      None => {
        println!("Usage: break 0x<addr>  OR  break <filename>:<line>");
        return Ok(());
      }
    };

    let addr = if !arg.starts_with("0x") && arg.contains(':') {
      match self.source_location_to_addr(arg)? {
        Some(addr) => addr,
        None => return Ok(()),
      }
    } else {
      parse_hex_u64(arg).with_context(|| format!("parse breakpoint address {arg}"))?
    };

    let breakpoint_addr = normalize_thumb_addr(addr);
    if self.breakpoints.contains(&breakpoint_addr) {
      println!("Breakpoint already exists at 0x{:08x}", breakpoint_addr);
      return Ok(());
    }

    self.insert_remote_breakpoint(breakpoint_addr)?;
    self.breakpoints.insert(breakpoint_addr);
    println!("Breakpoint set at 0x{:08x}", breakpoint_addr);
    Ok(())
  }

  /// Clear a tracked ARM breakpoint
  fn cmd_clear_breakpoint(&mut self, tokens: &[&str]) -> Result<()> {
    let arg = match tokens.get(1) {
      Some(arg) => *arg,
      None => {
        println!("Usage: clear 0x<addr>");
        return Ok(());
      }
    };

    let breakpoint_addr = normalize_thumb_addr(parse_hex_u64(arg)?);
    if !self.breakpoints.contains(&breakpoint_addr) {
      println!("No breakpoint tracked at 0x{:08x}", breakpoint_addr);
      return Ok(());
    }

    self.remove_remote_breakpoint(breakpoint_addr)?;
    self.breakpoints.remove(&breakpoint_addr);
    println!("Breakpoint cleared at 0x{:08x}", breakpoint_addr);
    Ok(())
  }

  /// Resolve a `file:line` argument into a code address
  fn source_location_to_addr(&self, arg: &str) -> Result<Option<u64>> {
    let parts: Vec<&str> = arg.rsplitn(2, ':').collect();
    if parts.len() != 2 {
      println!("Invalid format. Use: break <filename>:<line>");
      return Ok(None);
    }

    let line_num: u64 = match parts[0].parse() {
      Ok(line) => line,
      Err(_) => {
        println!("Invalid line number: {}", parts[0]);
        return Ok(None);
      }
    };
    let filename = parts[1];

    let addresses = self.debug_info.file_line_to_addr(filename, line_num)?;
    if addresses.is_empty() {
      println!("No code found at {}:{}", filename, line_num);
      return Ok(None);
    }

    Ok(Some(addresses[0]))
  }

  /// Handle ARM register dump/read/write commands
  fn cmd_register(&mut self, tokens: &[&str]) -> Result<()> {
    let subcmd = match tokens.get(1) {
      Some(subcmd) => *subcmd,
      None => {
        println!("Usage: register <dump|read|write> ...");
        return Ok(());
      }
    };

    match subcmd {
      "dump" => {
        for (desc, value) in self.read_arm_core_registers()? {
          println!("{:<10}  {:08x}", desc.name, value);
        }
      }
      "read" => {
        let Some(name) = tokens.get(2) else {
          println!("Usage: register read <reg_name>");
          return Ok(());
        };
        let Some(desc) = arm_register_from_name(name) else {
          println!("Invalid register name");
          return Ok(());
        };
        let value = self.read_register(desc.index)?;
        println!("{:<10}  {:08x}", desc.name, value);
      }
      "write" => {
        let Some(name) = tokens.get(2) else {
          println!("Usage: register write <reg_name> 0x<value>");
          return Ok(());
        };
        let Some(value) = tokens.get(3) else {
          println!("Usage: register write <reg_name> 0x<value>");
          return Ok(());
        };
        let Some(desc) = arm_register_from_name(name) else {
          println!("Invalid register name");
          return Ok(());
        };
        self.write_register(desc.index, parse_hex_u64(value)?)?;
      }
      _ => println!("Command not found"),
    }

    Ok(())
  }

  /// Handle ARM memory read/write commands
  fn cmd_memory(&mut self, tokens: &[&str]) -> Result<()> {
    let subcmd = match tokens.get(1) {
      Some(subcmd) => *subcmd,
      None => {
        println!("Usage: memory <read|write> 0x<addr> [len|value]");
        return Ok(());
      }
    };

    let Some(addr_str) = tokens.get(2) else {
      println!("Usage: memory <read|write> 0x<addr> [len|value]");
      return Ok(());
    };
    let addr = parse_hex_u64(addr_str)?;

    match subcmd {
      "read" => {
        let len = match tokens.get(3) {
          Some(len) => len.parse::<usize>().unwrap_or(4),
          None => 4,
        };
        let bytes = self.conn.read_memory(addr, len)?;
        print_hexdump(addr, &bytes);
      }
      "write" => {
        let Some(value) = tokens.get(3) else {
          println!("Usage: memory write 0x<addr> 0x<value>");
          return Ok(());
        };
        let data = u32_to_le_bytes(parse_hex_u64(value)? as u32);
        self.conn.write_memory(addr, &data)?;
        println!("Wrote 0x{} to 0x{:08x}", bytes_to_hex(&data), addr);
      }
      _ => println!("Invalid memory command"),
    }

    Ok(())
  }

  /// Send a monitor command to the GDB remote server
  fn cmd_monitor(&mut self, tokens: &[&str]) -> Result<()> {
    if tokens.len() < 2 {
      println!("Usage: monitor <openocd-command>");
      return Ok(());
    }

    self.monitor_command(&tokens[1..].join(" "))
  }

  /// Encode and send an OpenOCD monitor command
  fn monitor_command(&mut self, command: &str) -> Result<()> {
    let response = self
      .conn
      .send_packet(&format!("qRcmd,{}", bytes_to_hex(command.as_bytes())))?;
    self.print_monitor_response(response)
  }

  /// Print `qRcmd` monitor output packets until completion
  fn print_monitor_response(&mut self, mut response: String) -> Result<()> {
    loop {
      if is_console_output_packet(&response) {
        let output = response.strip_prefix('O').unwrap_or_default();
        print!("{}", String::from_utf8_lossy(&hex_to_bytes(output)?));
        response = self.conn.read_packet()?;
        continue;
      }

      expect_ok(&response)?;
      return Ok(());
    }
  }

  /// Interrupt the target and print its current location
  fn halt_target(&mut self) -> Result<()> {
    let reply = self.conn.interrupt()?;
    println!("{}", format_stop_reply(&reply));
    self.print_current_location()
  }

  /// Resolve the current PC to a source file and line
  fn get_current_line(&mut self) -> Result<Option<(String, u64)>> {
    let pc = self.arm_pc()?;
    self.debug_info.pc_to_file_line(pc)
  }

  /// Print current PC, location, and source line
  fn print_current_location(&mut self) -> Result<()> {
    let pc = self.arm_pc()?;
    println!("PC = 0x{:08x}", pc);
    println!("{}", self.format_location(pc));

    if let Some((file, line)) = self.debug_info.pc_to_file_line(pc)? {
      self.print_source(&file, line)?;
    }

    Ok(())
  }

  /// Print one source line from disk
  fn print_source(&self, path: &str, line: u64) -> Result<()> {
    let resolved_path = self.resolve_source_path(path);
    let contents = match std::fs::read_to_string(&resolved_path) {
      Ok(contents) => contents,
      Err(_) => {
        println!("Unable to read source file: {}", path);
        return Ok(());
      }
    };

    let idx = line.saturating_sub(1) as usize;
    match contents.lines().nth(idx) {
      Some(src_line) => println!("{}:{}\n\x1b[33m{}\x1b[0m", path, line, src_line),
      None => println!("{}:{} (line not found)", path, line),
    }

    Ok(())
  }
}

/// Print help for the ARM REPL
fn print_arm_help() {
  println!("Commands:");
  println!("  continue | c");
  println!("  step | s | next | n        source-level single step");
  println!("  stepi | si                 instruction single step");
  println!("  break 0x<addr>");
  println!("  break <filename>:<line>");
  println!("  clear 0x<addr>");
  println!("  register dump");
  println!("  register read <reg>");
  println!("  register write <reg> 0x<value>");
  println!("  memory read 0x<addr> [len]");
  println!("  memory write 0x<addr> 0x<value>");
  println!("  backtrace | bt");
  println!("  halt");
  println!("  reset");
  println!("  monitor <openocd-command>");
  println!("  quit | q");
}

/// Print a simple hex dump for ARM memory reads
fn print_hexdump(base_addr: u64, bytes: &[u8]) {
  for (line_index, chunk) in bytes.chunks(16).enumerate() {
    print!("{:08x}: ", base_addr + (line_index * 16) as u64);
    for byte in chunk {
      print!("{:02x} ", byte);
    }
    println!();
  }
}
