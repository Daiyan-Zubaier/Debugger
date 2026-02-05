use nix::Result;
use nix::libc::{SI_KERNEL, TRAP_BRKPT, TRAP_TRACE};
use nix::sys::ptrace::{self, AddressType};

use crate::debugger::Debugger;

impl Debugger {
  /// Handler to deal with SIGTRAP
  pub(crate) fn handle_sigtrap(&mut self) -> Result<()> {
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

        // Print source code
        if let Some((file, line)) = self.get_current_line() {
          let _ = self.print_source(&file, line);
        }

        if hit_bp {
          self.stepover_breakpoint()?;

          // Clean up temporary breakpoint if this was one
          if self.temp_breakpoints.contains(&bp_address) {
            self.cleanup_temp_breakpoint(bp_address)?;
          }
        }

        // Clean up temp breakpoints that are at or before our current position
        // (but keep return address breakpoints that are ahead of us)
        let current_pc = pc;
        let addrs_to_cleanup: Vec<_> = self
          .temp_breakpoints
          .iter()
          .filter(|&&addr| (addr as u64) <= current_pc)
          .copied()
          .collect();
        for addr in addrs_to_cleanup {
          self.cleanup_temp_breakpoint(addr)?;
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

  /// Handler to deal with SIGSEGV
  pub(crate) fn handle_sigsegv(&mut self) -> Result<()> {
    let sig_info = ptrace::getsiginfo(self.pid)?;

    println!(
      "Segfault! Reason Code: {}, Address {:?}",
      sig_info.si_code,
      unsafe { sig_info.si_addr() }
    );

    self.is_executing = false;
    self.has_crashed = true;

    println!(
      "Program has crashed. Use 'continue' to re-deliver the signal and terminate, or restart the debugger."
    );

    Ok(())
  }
}
