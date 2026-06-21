use anyhow::{Result as AnyResult, bail};
use nix::libc::{self, c_long};
use nix::sys::ptrace::{self, AddressType};
use nix::sys::signal::Signal;
use nix::sys::wait::{WaitStatus, waitpid};

use crate::target::{
  BreakpointView, DebugTarget, RegisterValue, SourceView, StackFrame, StopReason,
  empty_source_view, read_source_window,
};

use super::breakpoint::Breakpoint;
use super::debugger::LinuxPtraceDebugger;
use super::registers::{REG_DESCS, get_register_from_name, get_register_value, set_register_value};
impl LinuxPtraceDebugger {
  /// Wait for the Linux debuggee and convert the result into a shared stop reason
  fn wait_for_target_stop(&mut self) -> AnyResult<StopReason> {
    let status = waitpid(self.pid, None)?;
    self.handle_target_wait_status(status)
  }

  /// Convert a raw wait status into target state for the shared frontend
  fn handle_target_wait_status(&mut self, status: WaitStatus) -> AnyResult<StopReason> {
    match status {
      WaitStatus::Stopped(_, sig) => {
        self.is_executing = false;
        match sig {
          Signal::SIGTRAP => {
            self.handle_target_sigtrap()?;
            Ok(StopReason::Signal("SIGTRAP".to_string()))
          }
          Signal::SIGSEGV => {
            self.has_crashed = true;
            Ok(StopReason::Signal("SIGSEGV".to_string()))
          }
          other => Ok(StopReason::Signal(format!("{other:?}"))),
        }
      }
      WaitStatus::Exited(_, code) => Ok(StopReason::Exited(code)),
      WaitStatus::Signaled(_, sig, _) => Ok(StopReason::Terminated(format!("{sig:?}"))),
      other => Ok(StopReason::Other(format!("{other:?}"))),
    }
  }

  /// Handle breakpoint cleanup after a SIGTRAP in the target adapter path
  fn handle_target_sigtrap(&mut self) -> AnyResult<()> {
    let pc = self.get_pc()?;
    let bp_address = pc.saturating_sub(1) as AddressType;

    if self.breakpoints.contains_key(&bp_address) {
      self.stepover_breakpoint()?;

      if self.temp_breakpoints.contains(&bp_address) {
        self.cleanup_temp_breakpoint(bp_address)?;
      }
    }

    let addrs_to_cleanup: Vec<_> = self
      .temp_breakpoints
      .iter()
      .filter(|&&addr| (addr as u64) <= pc)
      .copied()
      .collect();
    for addr in addrs_to_cleanup {
      self.cleanup_temp_breakpoint(addr)?;
    }

    Ok(())
  }

  /// Convert user-entered addresses into runtime breakpoint addresses
  fn runtime_breakpoint_addr(&self, addr: u64) -> u64 {
    if addr < 0x400000 {
      self.to_runtime_addr(addr)
    } else {
      addr
    }
  }
}

impl DebugTarget for LinuxPtraceDebugger {
  /// Return the local program name
  fn name(&self) -> &str {
    &self.program_name
  }

  /// Return the backend architecture label
  fn architecture(&self) -> &str {
    "x86-64-linux-ptrace"
  }

  /// Wait for the initial post-exec stop
  fn initialize(&mut self) -> AnyResult<StopReason> {
    match waitpid(self.pid, None)? {
      WaitStatus::Stopped(_, _) => Ok(StopReason::Ready),
      other => Ok(StopReason::Other(format!("{other:?}"))),
    }
  }

  /// Continue the local process until the next stop
  fn continue_exec(&mut self) -> AnyResult<StopReason> {
    if self.has_crashed {
      bail!("program has crashed; continuing will terminate it");
    }

    let temp_addrs: Vec<_> = self.temp_breakpoints.iter().copied().collect();
    for addr in temp_addrs {
      self.cleanup_temp_breakpoint(addr)?;
    }

    ptrace::cont(self.pid, None)?;
    self.is_executing = true;
    self.wait_for_target_stop()
  }

  /// Single-step one x86-64 instruction
  fn step_instruction(&mut self) -> AnyResult<StopReason> {
    if self.has_crashed {
      bail!("program has crashed; restart the debugger");
    }

    ptrace::step(self.pid, None)?;
    self.wait_for_target_stop()
  }

  /// Report that asynchronous halting is unavailable for this backend
  fn halt(&mut self) -> AnyResult<StopReason> {
    Ok(StopReason::Other(
      "halt is not implemented for the local ptrace backend".to_string(),
    ))
  }

  /// Read the current instruction pointer
  fn pc(&mut self) -> AnyResult<u64> {
    Ok(self.get_pc()?)
  }

  /// Format a runtime PC for display
  fn location(&self, pc: u64) -> String {
    self.format_location(pc)
  }

  /// Load source context around the current PC
  fn current_source(&mut self, context_lines: usize) -> AnyResult<SourceView> {
    let Some((file, line)) = self.get_current_line() else {
      return Ok(empty_source_view());
    };

    let resolved_path = self.resolve_source_path(&file);
    let path = resolved_path.to_string_lossy().into_owned();
    read_source_window(&path, line, context_lines)
      .or_else(|_| read_source_window(&file, line, context_lines))
  }

  /// Read all x86-64 registers shown by the debugger
  fn registers(&mut self) -> AnyResult<Vec<RegisterValue>> {
    let mut values = Vec::new();
    for reg_desc in REG_DESCS.iter() {
      values.push(RegisterValue {
        name: reg_desc.name.to_string(),
        value: get_register_value(self.pid, reg_desc.reg)?,
      });
    }
    Ok(values)
  }

  /// Write an x86-64 register by name
  fn write_register_by_name(&mut self, name: &str, value: u64) -> AnyResult<()> {
    let Some(reg) = get_register_from_name(name) else {
      bail!("unknown register: {name}");
    };
    set_register_value(self.pid, reg, value)?;
    Ok(())
  }

  /// Read debuggee memory into bytes
  fn read_memory(&mut self, addr: u64, len: usize) -> AnyResult<Vec<u8>> {
    let word_size = std::mem::size_of::<c_long>();
    let mut bytes = Vec::with_capacity(len);
    let mut offset = 0usize;

    while offset < len {
      let word = ptrace::read(self.pid, (addr + offset as u64) as AddressType)?;
      bytes.extend_from_slice(&word.to_ne_bytes());
      offset += word_size;
    }

    bytes.truncate(len);
    Ok(bytes)
  }

  /// Write bytes to debuggee memory
  fn write_memory(&mut self, addr: u64, data: &[u8]) -> AnyResult<()> {
    let word_size = std::mem::size_of::<c_long>();

    for (chunk_index, chunk) in data.chunks(word_size).enumerate() {
      let write_addr = addr + (chunk_index * word_size) as u64;
      let mut word_bytes = if chunk.len() == word_size {
        [0u8; std::mem::size_of::<c_long>()]
      } else {
        ptrace::read(self.pid, write_addr as AddressType)?.to_ne_bytes()
      };
      word_bytes[..chunk.len()].copy_from_slice(chunk);
      let word = c_long::from_ne_bytes(word_bytes);
      ptrace::write(self.pid, write_addr as AddressType, word)?;
    }

    Ok(())
  }

  /// Insert an x86 software breakpoint
  fn set_breakpoint(&mut self, addr: u64) -> AnyResult<()> {
    let runtime_addr = self.runtime_breakpoint_addr(addr);
    let bp_addr = runtime_addr as AddressType;

    if self.breakpoints.contains_key(&bp_addr) {
      return Ok(());
    }

    let mut bp = Breakpoint::new(self.pid, bp_addr);
    bp.enable()?;
    self.breakpoints.insert(bp_addr, bp);
    Ok(())
  }

  /// Remove an x86 software breakpoint
  fn clear_breakpoint(&mut self, addr: u64) -> AnyResult<()> {
    let runtime_addr = self.runtime_breakpoint_addr(addr);
    let bp_addr = runtime_addr as AddressType;

    if let Some(mut bp) = self.breakpoints.remove(&bp_addr)
      && bp.enabled_status
    {
      bp.disable()?;
    }
    self.temp_breakpoints.remove(&bp_addr);
    Ok(())
  }

  /// Resolve a source line into a runtime breakpoint address
  fn breakpoint_addr_for_source(&self, file: &str, line: u64) -> AnyResult<Option<u64>> {
    let addresses = self.debug_info.file_line_to_addr(file, line)?;
    Ok(addresses.first().map(|addr| self.to_runtime_addr(*addr)))
  }

  /// Return current breakpoint state for frontends
  fn breakpoints(&self) -> Vec<BreakpointView> {
    self
      .breakpoints
      .iter()
      .map(|(addr, bp)| BreakpointView {
        addr: *addr as u64,
        temporary: self.temp_breakpoints.contains(addr),
        enabled: bp.enabled_status,
      })
      .collect()
  }

  /// Collect a frame-pointer based backtrace
  fn backtrace(&mut self) -> AnyResult<Vec<StackFrame>> {
    Ok(self.collect_backtrace()?)
  }

  /// Drain captured stdout/stderr from the debuggee without blocking the TUI
  fn drain_output(&mut self) -> AnyResult<Vec<u8>> {
    let Some(fd) = self.output_fd else {
      return Ok(Vec::new());
    };

    let mut output = Vec::new();
    let mut buf = [0u8; 4096];

    loop {
      let bytes_read = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
      if bytes_read > 0 {
        output.extend_from_slice(&buf[..bytes_read as usize]);
        continue;
      }

      if bytes_read == 0 {
        break;
      }

      let err = std::io::Error::last_os_error();
      match err.raw_os_error() {
        Some(code) if code == libc::EAGAIN || code == libc::EWOULDBLOCK => break,
        // Linux PTY masters return EIO after the slave side closes
        Some(code) if code == libc::EIO => break,
        _ => return Err(err.into()),
      }
    }

    Ok(output)
  }
}
