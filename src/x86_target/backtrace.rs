use nix::Result;
use nix::sys::ptrace::{self, AddressType};

use crate::target::StackFrame;

use super::debugger::LinuxPtraceDebugger;
use super::registers::{Register, get_register_value};

const MAX_BACKTRACE_FRAMES: usize = 64;

impl LinuxPtraceDebugger {
  /// Collect a best-effort x86-64 backtrace using the frame pointer chain
  pub(crate) fn collect_backtrace(&self) -> Result<Vec<StackFrame>> {
    let pc = self.get_pc()?;
    let mut frame_ptr = get_register_value(self.pid, Register::Rbp)?;
    let mut frames = Vec::new();

    frames.push(self.backtrace_frame(0, pc));

    let mut previous_frame_ptr = 0;
    for frame_index in 1..MAX_BACKTRACE_FRAMES {
      if !self.is_plausible_frame_pointer(frame_ptr, previous_frame_ptr) {
        break;
      }

      let next_frame_ptr = match self.read_u64(frame_ptr) {
        Ok(value) => value,
        Err(_) => break,
      };
      let return_addr = match self.read_u64(frame_ptr + 8) {
        Ok(value) => value,
        Err(_) => break,
      };

      if return_addr == 0 || !self.is_any_executable_address(return_addr) {
        break;
      }

      // Return addresses point just after the call instruction. Subtract one
      // byte so DWARF lookup tends to resolve the call site instead
      frames.push(self.backtrace_frame(frame_index, return_addr.saturating_sub(1)));

      previous_frame_ptr = frame_ptr;
      frame_ptr = next_frame_ptr;
    }

    Ok(frames)
  }

  /// Print a best-effort x86-64 backtrace using the frame pointer chain
  pub(crate) fn print_backtrace(&self) -> Result<()> {
    println!("Backtrace:");
    for frame in self.collect_backtrace()? {
      self.print_backtrace_frame(&frame);
    }

    Ok(())
  }

  /// Build a structured stack-frame entry for display layers
  fn backtrace_frame(&self, frame_index: usize, runtime_pc: u64) -> StackFrame {
    StackFrame {
      index: frame_index,
      pc: runtime_pc,
      location: self.format_location(runtime_pc),
      approximate: false,
    }
  }

  /// Print one collected stack frame
  fn print_backtrace_frame(&self, frame: &StackFrame) {
    println!("#{:<2} 0x{:016x} {}", frame.index, frame.pc, frame.location);
  }

  /// Read one pointer-sized value from the debuggee
  fn read_u64(&self, addr: u64) -> Result<u64> {
    Ok(ptrace::read(self.pid, addr as AddressType)? as u64)
  }

  /// Check whether a frame pointer can be safely followed
  fn is_plausible_frame_pointer(&self, frame_ptr: u64, previous_frame_ptr: u64) -> bool {
    frame_ptr != 0 && frame_ptr.is_multiple_of(8) && frame_ptr > previous_frame_ptr
  }
}
