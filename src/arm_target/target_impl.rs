use anyhow::{Context, Result, bail};

use crate::target::{
  BreakpointView, DebugTarget, RegisterValue, SourceView, StackFrame, StopReason,
  empty_source_view, read_source_window,
};

use super::debugger::{RemoteArmDebugger, normalize_thumb_addr};
use super::gdb_protocol::{bytes_to_hex, expect_ok, stop_reason_from_reply};
use super::registers::arm_register_from_name;

const ARM_SOURCE_STEP_LIMIT: usize = 256;

impl DebugTarget for RemoteArmDebugger {
  /// Return the firmware ELF name
  fn name(&self) -> &str {
    &self.program_name
  }

  /// Return the backend architecture label
  fn architecture(&self) -> &str {
    "arm-cortex-m-gdb-remote"
  }

  /// Query the remote target's initial stop state
  fn initialize(&mut self) -> Result<StopReason> {
    let (_supported, stop_reason) = self.remote_initial_stop()?;
    Ok(stop_reason)
  }

  /// Continue the remote target until it stops
  fn continue_exec(&mut self) -> Result<StopReason> {
    let reply = self.conn.cont()?;
    Ok(stop_reason_from_reply(&reply))
  }

  /// Single-step one remote instruction
  fn step_instruction(&mut self) -> Result<StopReason> {
    let pc = self.arm_pc()?;
    let breakpoint_at_pc = self.breakpoints.contains(&pc);

    if breakpoint_at_pc {
      self.remove_remote_breakpoint(pc)?;
    }

    let step_result = self.conn.step();

    if breakpoint_at_pc && let Err(restore_err) = self.insert_remote_breakpoint(pc) {
      return match step_result {
        Ok(_) => Err(restore_err),
        Err(step_err) => Err(step_err.context(format!(
          "also failed to restore breakpoint at 0x{pc:08x}: {restore_err}"
        ))),
      };
    }

    let reply = step_result?;
    Ok(stop_reason_from_reply(&reply))
  }

  /// Step instructions until the source location changes, with remote-stall protection
  fn step_source(&mut self) -> Result<StopReason> {
    let start_pc = self.arm_pc()?;
    let start_location = self.debug_info.pc_to_file_line(start_pc).ok().flatten();
    let mut previous_pc = start_pc;

    for _ in 0..ARM_SOURCE_STEP_LIMIT {
      let reason = self.step_instruction()?;
      if matches!(reason, StopReason::Exited(_) | StopReason::Terminated(_)) {
        return Ok(reason);
      }

      let current_pc = self.arm_pc()?;
      if current_pc == previous_pc {
        return Ok(StopReason::Other(format!(
          "Step did not advance PC from 0x{current_pc:08x}; target may still be stopped on a breakpoint"
        )));
      }

      let current_location = self.debug_info.pc_to_file_line(current_pc).ok().flatten();
      if current_location != start_location {
        return Ok(reason);
      }

      previous_pc = current_pc;
    }

    Ok(StopReason::Other(format!(
      "Source step limit reached after {ARM_SOURCE_STEP_LIMIT} instructions"
    )))
  }

  /// Execute the current source line, stepping over calls where possible
  fn next_source(&mut self) -> Result<StopReason> {
    let start_pc = self.arm_pc()?;
    let start_location = self.debug_info.pc_to_file_line(start_pc).ok().flatten();
    let Some(next_addr) = self.next_source_breakpoint_addr(start_pc, start_location)? else {
      return self.step_source();
    };

    let inserted_temp = !self.breakpoints.contains(&next_addr);
    if inserted_temp {
      self
        .insert_remote_breakpoint(next_addr)
        .with_context(|| format!("insert temporary next breakpoint at 0x{next_addr:08x}"))?;
    }

    let removed_current_breakpoint = self.breakpoints.contains(&start_pc);
    if removed_current_breakpoint {
      if let Err(remove_err) = self
        .remove_remote_breakpoint(start_pc)
        .with_context(|| format!("temporarily remove breakpoint at 0x{start_pc:08x}"))
      {
        if inserted_temp && let Err(cleanup_err) = self.remove_remote_breakpoint(next_addr) {
          return Err(remove_err.context(format!(
            "also failed to remove temporary next breakpoint at 0x{next_addr:08x}: {cleanup_err}"
          )));
        }
        return Err(remove_err);
      }
    }

    let continue_result = self.conn.cont();

    let cleanup_result = if inserted_temp {
      self
        .remove_remote_breakpoint(next_addr)
        .with_context(|| format!("remove temporary next breakpoint at 0x{next_addr:08x}"))
    } else {
      Ok(())
    };

    let restore_result = if removed_current_breakpoint {
      self
        .insert_remote_breakpoint(start_pc)
        .with_context(|| format!("restore breakpoint at 0x{start_pc:08x}"))
    } else {
      Ok(())
    };

    let reply = continue_result?;
    cleanup_result?;
    restore_result?;
    Ok(stop_reason_from_reply(&reply))
  }

  /// Interrupt the remote target
  fn halt(&mut self) -> Result<StopReason> {
    let reply = self.conn.interrupt()?;
    Ok(stop_reason_from_reply(&reply))
  }

  /// Reset and halt the remote target through OpenOCD
  fn reset_halt(&mut self) -> Result<StopReason> {
    let command = bytes_to_hex(b"reset halt");
    let response = self.conn.send_packet(&format!("qRcmd,{command}"))?;
    expect_ok(&response)?;
    Ok(StopReason::Other("Target reset and halted".to_string()))
  }

  /// Read the current normalized ARM PC
  fn pc(&mut self) -> Result<u64> {
    self.arm_pc()
  }

  /// Format an ARM code address for display
  fn location(&self, pc: u64) -> String {
    self.format_location(pc)
  }

  /// Load source context around the current ARM PC
  fn current_source(&mut self, context_lines: usize) -> Result<SourceView> {
    let pc = self.arm_pc()?;
    let Some((file, line)) = self.debug_info.pc_to_file_line(pc)? else {
      return Ok(empty_source_view());
    };

    let resolved_path = self.resolve_source_path(&file);
    let path = resolved_path.to_string_lossy().into_owned();
    read_source_window(&path, line, context_lines)
      .or_else(|_| read_source_window(&file, line, context_lines))
      .or_else(|_| {
        Ok(SourceView {
          path: Some(file),
          current_line: Some(line),
          lines: vec![],
        })
      })
  }

  /// Read ARM core registers for the frontend
  fn registers(&mut self) -> Result<Vec<RegisterValue>> {
    Ok(
      self
        .read_arm_core_registers()?
        .into_iter()
        .map(|(desc, value)| RegisterValue {
          name: desc.name.to_string(),
          value,
        })
        .collect(),
    )
  }

  /// Write an ARM core register by name
  fn write_register_by_name(&mut self, name: &str, value: u64) -> Result<()> {
    let Some(desc) = arm_register_from_name(name) else {
      bail!("unknown register: {name}");
    };
    self.write_register(desc.index, value)
  }

  /// Read remote target memory into bytes
  fn read_memory(&mut self, addr: u64, len: usize) -> Result<Vec<u8>> {
    self.conn.read_memory(addr, len)
  }

  /// Write bytes to remote target memory
  fn write_memory(&mut self, addr: u64, data: &[u8]) -> Result<()> {
    self.conn.write_memory(addr, data)
  }

  /// Insert a remote ARM breakpoint
  fn set_breakpoint(&mut self, addr: u64) -> Result<()> {
    let breakpoint_addr = normalize_thumb_addr(addr);
    if self.breakpoints.contains(&breakpoint_addr) {
      return Ok(());
    }

    self.insert_remote_breakpoint(breakpoint_addr)?;
    self.breakpoints.insert(breakpoint_addr);
    Ok(())
  }

  /// Remove a remote ARM breakpoint
  fn clear_breakpoint(&mut self, addr: u64) -> Result<()> {
    let breakpoint_addr = normalize_thumb_addr(addr);
    if !self.breakpoints.contains(&breakpoint_addr) {
      return Ok(());
    }

    self.remove_remote_breakpoint(breakpoint_addr)?;
    self.breakpoints.remove(&breakpoint_addr);
    Ok(())
  }

  /// Resolve a source line into a normalized ARM breakpoint address
  fn breakpoint_addr_for_source(&self, file: &str, line: u64) -> Result<Option<u64>> {
    let addresses = self.debug_info.file_line_to_addr(file, line)?;
    Ok(addresses.first().map(|addr| normalize_thumb_addr(*addr)))
  }

  /// Return current remote breakpoint state for frontends
  fn breakpoints(&self) -> Vec<BreakpointView> {
    self
      .breakpoints
      .iter()
      .map(|addr| BreakpointView {
        addr: *addr,
        temporary: false,
        enabled: true,
      })
      .collect()
  }

  /// Collect the ARM stack-scan backtrace
  fn backtrace(&mut self) -> Result<Vec<StackFrame>> {
    self.collect_backtrace()
  }
}

impl RemoteArmDebugger {
  /// Choose the next source location in the current function for a temporary breakpoint.
  fn next_source_breakpoint_addr(
    &mut self,
    start_pc: u64,
    start_location: Option<(String, u64)>,
  ) -> Result<Option<u64>> {
    let Some(start_location) = start_location else {
      return Ok(None);
    };

    let Some(func_ranges) = self.debug_info.get_function_ranges(start_pc)? else {
      return Ok(None);
    };

    let mut candidates = self
      .debug_info
      .get_line_addresses_in_ranges(&func_ranges, start_pc)?
      .into_iter()
      .map(normalize_thumb_addr)
      .filter(|addr| *addr > start_pc)
      .collect::<Vec<_>>();
    candidates.sort_unstable();
    candidates.dedup();

    for addr in candidates {
      let location = self.debug_info.pc_to_file_line(addr).ok().flatten();
      if location.as_ref() != Some(&start_location) {
        return Ok(Some(addr));
      }
    }

    let return_addr = normalize_thumb_addr(self.read_register(14)?);
    if self.debug_info.pc_to_function(return_addr)?.is_some() {
      return Ok(Some(return_addr));
    }

    Ok(None)
  }
}
