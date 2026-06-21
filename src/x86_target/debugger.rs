use std::collections::{HashMap, HashSet};
use std::os::fd::RawFd;
use std::path::{Path, PathBuf};

use nix::Result;
use nix::libc;
use nix::sys::ptrace::{self, AddressType};
use nix::sys::signal::Signal;
use nix::sys::wait::{WaitStatus, waitpid};
use nix::unistd::Pid;

use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;

use crate::elf_debug_info::ElfDebugInfo;

use super::breakpoint::Breakpoint;
use super::registers::{Register, get_register_value, set_register_value};

#[derive(Debug, Eq, PartialEq)]
struct ProcessMapEntry {
  start: u64,
  end: u64,
  offset: u64,
  path: Option<String>,
}

#[derive(Debug, Eq, PartialEq)]
struct MappedObject {
  name: String,
  offset: u64,
}

/// State for a local Linux `ptrace` debugging session
pub struct LinuxPtraceDebugger {
  /// Path to program
  pub(crate) program_name: String,

  /// Program will run on child process, this is the pid for that
  pub(crate) pid: Pid,

  /// Flag to see if program is executing
  pub(crate) is_executing: bool,

  /// Flag to indicate the program has crashed (received a fatal signal like SIGSEGV)
  pub(crate) has_crashed: bool,

  /// Active breakpoints keyed by runtime address
  pub(crate) breakpoints: HashMap<AddressType, Breakpoint>,

  /// Temporary breakpoints to remove after being hit (used by step_out, step_over)
  pub(crate) temp_breakpoints: HashSet<AddressType>,

  /// ELF/DWARF debug information for source-level lookups
  pub(crate) debug_info: ElfDebugInfo,

  /// Optional nonblocking fd used by the TUI to capture debuggee stdout/stderr
  pub(crate) output_fd: Option<RawFd>,
}

impl LinuxPtraceDebugger {
  /// Construct a debugger for a forked Linux child process
  pub fn new(program_name: String, pid: Pid) -> Self {
    Self::new_with_output_fd(program_name, pid, None)
  }

  /// Construct a debugger with an optional captured-output file descriptor
  pub fn new_with_output_fd(program_name: String, pid: Pid, output_fd: Option<RawFd>) -> Self {
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
      output_fd,
    }
  }

  /// Run the non-tui Linux debugger REPL
  pub fn run(&mut self) -> rustyline::Result<()> {
    // Wait for the child to stop after ptrace::traceme + execvp
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
    if let Err(e) = self.cleanup_all_temp_breakpoints() {
      println!("Failed to clean up temporary breakpoints: {:?}", e);
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
    println!("{}", self.format_location(runtime_pc));
  }

  /// Format a runtime address as function and source location text
  pub(crate) fn format_location(&self, runtime_pc: u64) -> String {
    let dwarf_pc = self.to_dwarf_pc(runtime_pc);
    let func = self.debug_info.pc_to_function(dwarf_pc).ok().flatten();
    let loc = self.debug_info.pc_to_file_line(dwarf_pc).ok().flatten();

    match (func, loc) {
      (Some(f), Some((file, line))) => format!("At {f} ({file}:{line})"),
      (Some(f), None) => format!("At {f} (no line info)"),
      (None, Some((file, line))) => format!("At {file}:{line}"),
      (None, None) => self
        .mapped_object_for_addr(runtime_pc)
        .map(|object| {
          format!(
            "At 0x{runtime_pc:x} ({} + 0x{:x}, no DWARF match)",
            object.name, object.offset
          )
        })
        .unwrap_or_else(|| format!("At 0x{runtime_pc:x} (no DWARF match)")),
    }
  }

  /// Resolve a runtime address to the mapped object that owns it
  fn mapped_object_for_addr(&self, runtime_addr: u64) -> Option<MappedObject> {
    let maps_path = format!("/proc/{}/maps", self.pid);
    let maps = std::fs::read_to_string(maps_path).ok()?;
    mapped_object_for_addr(&maps, runtime_addr)
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

  /// Check if an address is in an executable code region of our program
  /// Returns false for stack addresses, heap, libraries, etc
  pub(crate) fn is_valid_code_address(&self, addr: u64) -> bool {
    self.is_executable_address_in_program(addr, true)
  }

  /// Check if an address is in ANY executable code region (including libraries)
  /// Use this for validating return addresses which may be in libc
  pub(crate) fn is_any_executable_address(&self, addr: u64) -> bool {
    self.is_executable_address_in_program(addr, false)
  }

  /// Helper: Check if address is in an executable region
  /// If `our_program_only` is true, only accepts our program's code
  fn is_executable_address_in_program(&self, addr: u64, our_program_only: bool) -> bool {
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

      // Check if address is in this executable range
      if addr >= start && addr < end {
        if !our_program_only {
          return true; // Any executable region is valid
        }
        // Only our program's code
        if let Some(p) = path
          && (p.ends_with(&self.program_name) || p.ends_with(prog_base))
        {
          return true;
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
    let resolved_path = self.resolve_source_path(path);

    if let Ok(contents) = std::fs::read_to_string(&resolved_path) {
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

  /// Resolve source paths from DWARF against the debuggee directory when needed
  pub(crate) fn resolve_source_path(&self, path: &str) -> PathBuf {
    let source_path = Path::new(path);
    if source_path.exists() || source_path.is_absolute() {
      return source_path.to_path_buf();
    }

    if let Ok(current_dir) = std::env::current_dir() {
      let candidate = current_dir.join(source_path);
      if candidate.exists() {
        return candidate;
      }
    }

    if let Some(program_dir) = Path::new(&self.program_name).parent() {
      let candidate = program_dir.join(source_path);
      if candidate.exists() {
        return candidate;
      }

      if let Some(parent_dir) = program_dir.parent() {
        let candidate = parent_dir.join(source_path);
        if candidate.exists() {
          return candidate;
        }
      }
    }

    source_path.to_path_buf()
  }

  /// stepi instruction - single step one instruction
  pub(crate) fn single_step(&mut self) -> Result<()> {
    if self.has_crashed {
      println!(
        "Cannot step: program has crashed. Use 'continue' to terminate or restart the debugger."
      );
      return Ok(());
    }

    // Note: Breakpoint handling is done by the signal handler when we hit one
    // Here we just need to step the instruction
    ptrace::step(self.pid, None)?;
    Ok(())
  }
}

fn mapped_object_for_addr(maps: &str, runtime_addr: u64) -> Option<MappedObject> {
  let entry = maps
    .lines()
    .filter_map(parse_process_map_entry)
    .find(|entry| runtime_addr >= entry.start && runtime_addr < entry.end)?;

  let name = entry
    .path
    .as_deref()
    .map(map_path_label)
    .unwrap_or_else(|| "anonymous mapping".to_string());
  let offset = runtime_addr.saturating_sub(entry.start) + entry.offset;

  Some(MappedObject { name, offset })
}

fn parse_process_map_entry(line: &str) -> Option<ProcessMapEntry> {
  let mut parts = line.split_whitespace();
  let range = parts.next()?;
  let _perms = parts.next()?;
  let offset_hex = parts.next()?;
  let _dev = parts.next()?;
  let _inode = parts.next()?;
  let path = parts.next().map(str::to_string);

  let (start_hex, end_hex) = range.split_once('-')?;
  let start = u64::from_str_radix(start_hex, 16).ok()?;
  let end = u64::from_str_radix(end_hex, 16).ok()?;
  let offset = u64::from_str_radix(offset_hex, 16).ok()?;

  Some(ProcessMapEntry {
    start,
    end,
    offset,
    path,
  })
}

fn map_path_label(path: &str) -> String {
  Path::new(path)
    .file_name()
    .and_then(|name| name.to_str())
    .unwrap_or(path)
    .to_string()
}

impl Drop for LinuxPtraceDebugger {
  fn drop(&mut self) {
    if let Some(fd) = self.output_fd.take() {
      unsafe {
        libc::close(fd);
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::{MappedObject, mapped_object_for_addr, parse_process_map_entry};

  #[test]
  fn parses_process_map_entry() {
    let entry = parse_process_map_entry(
      "7ffff7dcf000-7ffff7f24000 r-xp 00026000 08:01 123 /usr/lib/libc.so.6",
    )
    .unwrap();

    assert_eq!(entry.start, 0x7ffff7dcf000);
    assert_eq!(entry.end, 0x7ffff7f24000);
    assert_eq!(entry.offset, 0x26000);
    assert_eq!(entry.path.as_deref(), Some("/usr/lib/libc.so.6"));
  }

  #[test]
  fn resolves_mapped_object_offset() {
    let maps = "7ffff7dcf000-7ffff7f24000 r-xp 00026000 08:01 123 /usr/lib/libc.so.6\n";

    assert_eq!(
      mapped_object_for_addr(maps, 0x7ffff7df7249),
      Some(MappedObject {
        name: "libc.so.6".to_string(),
        offset: 0x4e249,
      })
    );
  }
}
