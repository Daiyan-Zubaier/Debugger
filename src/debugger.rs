use std::collections::{HashMap, HashSet};

use nix::Result;
use nix::sys::ptrace::{self, AddressType};
use nix::sys::signal::Signal;
use nix::sys::wait::{WaitStatus, waitpid};
use nix::unistd::Pid;

use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;

use crate::breakpoint::Breakpoint;
use crate::elf_debug_info::ElfDebugInfo;
use crate::registers::{Register, get_register_value, set_register_value};

// Core debugger Module

/// Struct to hold debugger information
pub struct Debugger {
  /// Path to program
  pub(crate) program_name: String,

  /// Program will run on child process, this is the pid for that
  pub(crate) pid: Pid,

  /// Flag to see if program is executing
  pub(crate) is_executing: bool,

  /// Flag to indicate the program has crashed (received a fatal signal like SIGSEGV)
  pub(crate) has_crashed: bool,

  /// A hash map of all breakpoints
  pub(crate) breakpoints: HashMap<AddressType, Breakpoint>,

  /// Temporary breakpoints to remove after being hit (used by step_out, step_over)
  pub(crate) temp_breakpoints: HashSet<AddressType>,

  /// AN object to hold debug info
  pub(crate) debug_info: ElfDebugInfo,
}

impl Debugger {
  /// Construct new Debugger object
  pub fn new(program_name: String, pid: Pid) -> Self {
    let debug_info =
      ElfDebugInfo::new(program_name.clone()).expect("Error constructing ElfDebugInfo"); // expect used since unrecoverable state

    Self {
      program_name,
      pid,
      is_executing: false,
      has_crashed: false,
      breakpoints: HashMap::new(),
      temp_breakpoints: HashSet::new(),
      debug_info,
    }
  }

  /// Run the debugger
  pub fn run(&mut self) -> rustyline::Result<()> {
    // For now Option is set to None. This means it only blocks until child exits or is killed.
    // options is a bitmask that determines which state transitions to block
    // Waits for thread to be ready
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

  /// Step over breakpoint
  pub(crate) fn stepover_breakpoint(&mut self) -> Result<()> {
    let current_pc = self.get_pc()?;
    let possible_bp_location = (current_pc - 1) as AddressType;

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
  pub(crate) fn get_pc(&self) -> Result<u64> {
    get_register_value(self.pid, Register::Rip)
  }

  /// Set program counter
  pub(crate) fn set_pc(&self, value: u64) -> Result<()> {
    set_register_value(self.pid, Register::Rip, value)
  }

  /// Prints location
  pub(crate) fn print_location(&self, runtime_pc: u64) {
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

  /// Loads bias from /proc/pid/maps
  pub(crate) fn load_bias(&self) -> std::io::Result<u64> {
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

      return Ok(start.saturating_sub(offset));
    }

    Err(std::io::Error::new(
      std::io::ErrorKind::NotFound,
      "could not determine load bias from /proc/<pid>/maps",
    ))
  }

  /// Check if an address is in an executable code region of our program.
  /// Returns false for stack addresses, heap, libraries, etc.
  pub(crate) fn is_valid_code_address(&self, addr: u64) -> bool {
    let maps_path = format!("/proc/{}/maps", self.pid);
    let maps = match std::fs::read_to_string(&maps_path) {
      Ok(m) => m,
      Err(_) => return false,
    };

    let prog_base = self
      .program_name
      .rsplit('/')
      .next()
      .unwrap_or(&self.program_name);

    for line in maps.lines() {
      let mut it = line.split_whitespace();

      let range = match it.next() {
        Some(v) => v,
        None => continue,
      };
      let perms = match it.next() {
        Some(v) => v,
        None => continue,
      };
      let _offset = it.next();
      let _dev = it.next();
      let _inode = it.next();
      let path = it.next();

      // Only consider executable regions
      if !perms.contains('x') {
        continue;
      }

      // Parse start-end range
      let mut range_parts = range.split('-');
      let start_hex = match range_parts.next() {
        Some(v) => v,
        None => continue,
      };
      let end_hex = match range_parts.next() {
        Some(v) => v,
        None => continue,
      };

      let start = match u64::from_str_radix(start_hex, 16) {
        Ok(v) => v,
        Err(_) => continue,
      };
      let end = match u64::from_str_radix(end_hex, 16) {
        Ok(v) => v,
        Err(_) => continue,
      };

      // Check if address is in this range AND it's our program (not a library)
      if addr >= start && addr < end {
        if let Some(p) = path {
          if p.ends_with(&self.program_name) || p.ends_with(prog_base) {
            return true;
          }
        }
      }
    }

    false
  }

  /// Convert from pc to dwarf mapped pc
  pub(crate) fn to_dwarf_pc(&self, runtime_pc: u64) -> u64 {
    match self.load_bias() {
      Ok(bias) => runtime_pc.saturating_sub(bias),
      Err(_) => runtime_pc, // fallback: non-PIE or couldn't read maps
    }
  }

  /// Convert DWARF mapped address to runtime address
  pub(crate) fn to_runtime_addr(&self, dwarf_addr: u64) -> u64 {
    match self.load_bias() {
      Ok(bias) => bias.saturating_add(dwarf_addr),
      Err(_) => dwarf_addr,
    }
  }

  /// Get current file and line
  pub(crate) fn get_current_line(&self) -> Option<(String, u64)> {
    let pc = self.get_pc().unwrap_or_else(|err| {
      println!("ERROR getting current line: {:?}", err);
      0
    });

    let dwarf_pc = self.to_dwarf_pc(pc);

    self
      .debug_info
      .pc_to_file_line(dwarf_pc)
      .unwrap_or_else(|err| {
        println!("ERROR getting file line {:?}", err);
        None
      })
  }

  /// Print a single source line given file path and line number
  pub(crate) fn print_source(&self, path: &str, line: u64) -> Result<()> {
    use std::fs;
    use std::path::Path;

    // Try the path as is first
    let mut resolved_path = Path::new(path).to_path_buf();

    // If it doesn't exist and is relative, try resolving relative to program directory
    if !resolved_path.exists() && !path.starts_with('/') {
      if let Some(program_dir) = Path::new(&self.program_name).parent() {
        let candidate = program_dir.join(path);
        if candidate.exists() {
          resolved_path = candidate;
        }
      }
    }

    if let Ok(contents) = fs::read_to_string(&resolved_path) {
      let idx = line.saturating_sub(1) as usize;
      if let Some(src_line) = contents.lines().nth(idx) {
        // Yellow text: \x1b[33m, Reset: \x1b[0m
        println!("{}:{}\n\x1b[33m{}\x1b[0m", path, line, src_line);
      } else {
        println!("{}:{} (line not found)", path, line);
      }
    } else {
      println!("Unable to read source file: {}", path);
    }
    Ok(())
  }

  /// stepi instruction - single step one instruction
  pub(crate) fn single_step(&mut self) -> Result<()> {
    if self.has_crashed {
      println!(
        "Cannot step: program has crashed. Use 'continue' to terminate or restart the debugger."
      );
      return Ok(());
    }

    // Note: Breakpoint handling is done by the signal handler when we hit one.
    // Here we just need to step the instruction.
    ptrace::step(self.pid, None)?;
    Ok(())
  }
}
