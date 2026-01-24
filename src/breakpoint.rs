use nix::sys::ptrace::{self, AddressType};
use nix::unistd::Pid;

pub struct Breakpoint {
  pid: Pid,
  addr: AddressType,
  pub enabled_status: bool,
  pub saved_data: u8,
}

const BREAKPOINT_OPCODE: i64 = 0xCC;

impl Breakpoint {
  pub fn new(pid: Pid, addr: AddressType) -> Self {
    Self {
      pid,
      addr,
      enabled_status: false,
      saved_data: 0,
    }
  }

  pub fn enable(&mut self) -> nix::Result<()> {
    let data = ptrace::read(self.pid, self.addr)?;
    self.saved_data = data as u8; // Cast automatically keeps last byte
    let written_data = (data & !0xFF) | BREAKPOINT_OPCODE;
    ptrace::write(
      self.pid,
      self.addr,
      written_data, // Replace last byte with breakpoint opcode
    )?;
    self.enabled_status = true;
    Ok(())
  }

  pub fn disable(&mut self) -> nix::Result<()> {
    let breakpoint_data = ptrace::read(self.pid, self.addr)?;
    let saved_data = self.saved_data as i64;

    ptrace::write(self.pid, self.addr, (breakpoint_data & !0xFF) | saved_data)?;

    self.enabled_status = false;
    Ok(())
  }
}
