use std::collections::HashSet;

use anyhow::Result;

use crate::target::StackFrame;

use super::debugger::{RemoteArmDebugger, normalize_thumb_addr};

const MAX_ARM_BACKTRACE_FRAMES: usize = 16;
const MAX_ARM_STACK_SCAN_WORDS: usize = 512;

impl RemoteArmDebugger {
  /// Print the current ARM backtrace
  pub(super) fn print_backtrace(&mut self) -> Result<()> {
    println!("Backtrace:");
    let frames = self.collect_backtrace()?;
    for frame in &frames {
      print_arm_backtrace_frame(frame);
    }

    if frames.len() == 1 {
      println!(
        "No caller frames found by stack scan. Build with frame pointers or unwind info for better ARM backtraces."
      );
    }

    Ok(())
  }

  /// Collect a stack-scan based ARM backtrace
  pub(super) fn collect_backtrace(&mut self) -> Result<Vec<StackFrame>> {
    let pc = self.arm_pc()?;
    let sp = self.arm_sp()?;
    let mut frames = vec![self.arm_backtrace_frame(0, pc, false)];

    let mut seen = HashSet::new();
    let mut frame_index = 1;
    let stack_len = MAX_ARM_STACK_SCAN_WORDS * 4;
    let stack = self.conn.read_memory(sp, stack_len).unwrap_or_default();

    for chunk in stack.chunks_exact(4) {
      let word = u64::from(u32::from_le_bytes(chunk.try_into()?));

      let Some(candidate_pc) = self.resolve_thumb_return_address(word) else {
        continue;
      };
      if !seen.insert(candidate_pc) {
        continue;
      }

      frames.push(self.arm_backtrace_frame(frame_index, candidate_pc, true));
      frame_index += 1;
      if frame_index >= MAX_ARM_BACKTRACE_FRAMES {
        break;
      }
    }

    Ok(frames)
  }

  /// Treat odd stack words as possible Thumb return addresses
  fn resolve_thumb_return_address(&self, value: u64) -> Option<u64> {
    if value & 1 == 0 {
      return None;
    }

    let addr = normalize_thumb_addr(value);
    [addr, addr.saturating_sub(2), addr.saturating_sub(4)]
      .into_iter()
      .find(|&pc| self.debug_info.pc_to_function(pc).ok().flatten().is_some())
  }

  /// Build one ARM backtrace frame
  fn arm_backtrace_frame(&self, frame_index: usize, pc: u64, approximate: bool) -> StackFrame {
    StackFrame {
      index: frame_index,
      pc,
      location: self.format_location(pc),
      approximate,
    }
  }
}

/// Print one ARM backtrace frame
fn print_arm_backtrace_frame(frame: &StackFrame) {
  let suffix = if frame.approximate {
    " [stack scan]"
  } else {
    ""
  };
  println!(
    "#{:<2} 0x{:08x} {}{}",
    frame.index, frame.pc, frame.location, suffix
  );
}
