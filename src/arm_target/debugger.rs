use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::elf_debug_info::ElfDebugInfo;
use crate::target::StopReason;

use super::gdb_protocol::{
  GdbRemoteConnection, bytes_to_hex, expect_ok, hex_to_bytes, parse_le_u32_hex,
  stop_reason_from_reply, u32_to_le_bytes,
};
use super::registers::{ARM_CORE_REGS, ArmRegisterDesc};

const DEFAULT_REMOTE_ADDR: &str = "127.0.0.1:3333";
const ARM_BREAKPOINT_KIND_THUMB16: u8 = 2;

pub struct RemoteArmDebugger {
  pub(super) program_name: String,
  pub(super) endpoint: String,
  pub(super) conn: GdbRemoteConnection,
  pub(super) debug_info: ElfDebugInfo,
  pub(super) breakpoints: HashSet<u64>,
}

impl RemoteArmDebugger {
  pub fn connect(program_name: String, endpoint: Option<String>) -> Result<Self> {
    let endpoint = endpoint.unwrap_or_else(|| DEFAULT_REMOTE_ADDR.to_string());
    let debug_info = ElfDebugInfo::new(program_name.clone())?;
    let conn = GdbRemoteConnection::connect(&endpoint)?;

    Ok(Self {
      program_name,
      endpoint,
      conn,
      debug_info,
      breakpoints: HashSet::new(),
    })
  }

  /// Ask the GDB remote server for feature and stop-state information
  pub(super) fn remote_initial_stop(&mut self) -> Result<(String, StopReason)> {
    let supported = self.conn.send_packet("qSupported")?;
    let stop_reply = self.conn.send_packet("?")?;
    Ok((supported, stop_reason_from_reply(&stop_reply)))
  }

  /// Insert a hardware breakpoint, falling back to software if needed
  pub(super) fn insert_remote_breakpoint(&mut self, addr: u64) -> Result<()> {
    let response = self
      .conn
      .insert_breakpoint("Z1", addr, ARM_BREAKPOINT_KIND_THUMB16)?;
    if response == "OK" {
      return Ok(());
    }

    let fallback = self
      .conn
      .insert_breakpoint("Z0", addr, ARM_BREAKPOINT_KIND_THUMB16)?;
    expect_ok(&fallback)
  }

  /// Remove a hardware breakpoint, falling back to software if needed
  pub(super) fn remove_remote_breakpoint(&mut self, addr: u64) -> Result<()> {
    let response = self
      .conn
      .remove_breakpoint("z1", addr, ARM_BREAKPOINT_KIND_THUMB16)?;
    if response == "OK" {
      return Ok(());
    }

    let fallback = self
      .conn
      .remove_breakpoint("z0", addr, ARM_BREAKPOINT_KIND_THUMB16)?;
    expect_ok(&fallback)
  }

  /// Read one ARM core register by GDB register index
  pub(super) fn read_register(&mut self, index: usize) -> Result<u64> {
    let response = self.conn.send_packet(&format!("p{:x}", index))?;
    if response.starts_with('E') || response.is_empty() {
      let regs = self.read_arm_core_registers()?;
      let Some((_desc, value)) = regs.into_iter().find(|(desc, _)| desc.index == index) else {
        bail!(
          "register index {} was not in the core register packet",
          index
        );
      };
      return Ok(value);
    }

    parse_le_u32_hex(&response).map(u64::from)
  }

  /// Write one ARM core register by GDB register index
  pub(super) fn write_register(&mut self, index: usize, value: u64) -> Result<()> {
    let bytes = u32_to_le_bytes(value as u32);
    let response = self
      .conn
      .send_packet(&format!("P{:x}={}", index, bytes_to_hex(&bytes)))?;
    expect_ok(&response)
  }

  /// Read the Cortex-M core register block
  pub(super) fn read_arm_core_registers(&mut self) -> Result<Vec<(ArmRegisterDesc, u64)>> {
    let response = self.conn.send_packet("g")?;
    if response.starts_with('E') {
      bail!("remote target rejected register read: {}", response);
    }

    let bytes = hex_to_bytes(&response)?;
    let mut regs = Vec::new();
    for desc in ARM_CORE_REGS {
      let start = desc.index * 4;
      let Some(chunk) = bytes.get(start..start + 4) else {
        break;
      };
      regs.push((desc, u64::from(u32::from_le_bytes(chunk.try_into()?))));
    }

    Ok(regs)
  }

  /// Read and normalize the ARM program counter
  pub(super) fn arm_pc(&mut self) -> Result<u64> {
    Ok(normalize_thumb_addr(self.read_register(15)?))
  }

  /// Read the ARM stack pointer
  pub(super) fn arm_sp(&mut self) -> Result<u64> {
    self.read_register(13)
  }

  /// Format a target address using DWARF function and line info
  pub(super) fn format_location(&self, pc: u64) -> String {
    let func = self.debug_info.pc_to_function(pc).ok().flatten();
    let loc = self.debug_info.pc_to_file_line(pc).ok().flatten();

    match (func, loc) {
      (Some(func), Some((file, line))) => format!("At {func} ({file}:{line})"),
      (Some(func), None) => format!("At {func} (no line info)"),
      (None, Some((file, line))) => format!("At {file}:{line}"),
      (None, None) => format!("At 0x{:x} (no DWARF match)", pc),
    }
  }

  /// Resolve source paths from DWARF against common firmware project locations
  pub(super) fn resolve_source_path(&self, path: &str) -> PathBuf {
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
}

/// Clear Thumb state bit from an address
pub(super) fn normalize_thumb_addr(addr: u64) -> u64 {
  addr & !1
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn normalizes_thumb_addresses() {
    assert_eq!(normalize_thumb_addr(0x08000101), 0x08000100);
  }
}
