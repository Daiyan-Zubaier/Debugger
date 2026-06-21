use std::io::{ErrorKind, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};

use crate::target::StopReason;

const COMMAND_READ_TIMEOUT: Duration = Duration::from_secs(30);

pub(super) struct GdbRemoteConnection {
  stream: TcpStream,
}

impl GdbRemoteConnection {
  /// Connect to a GDB remote TCP endpoint
  pub(super) fn connect(endpoint: &str) -> Result<Self> {
    let addr = endpoint
      .to_socket_addrs()
      .with_context(|| format!("resolve GDB remote endpoint {endpoint}"))?
      .next()
      .ok_or_else(|| anyhow!("no socket addresses resolved for {endpoint}"))?;
    let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(3))
      .with_context(|| format!("connect to GDB remote {endpoint}"))?;

    stream.set_read_timeout(Some(COMMAND_READ_TIMEOUT))?;
    stream.set_write_timeout(Some(COMMAND_READ_TIMEOUT))?;

    Ok(Self { stream })
  }

  /// Send a continue packet
  pub(super) fn cont(&mut self) -> Result<String> {
    self.with_read_timeout(None, |conn| conn.send_packet("c"))
  }

  /// Send an instruction-step packet
  pub(super) fn step(&mut self) -> Result<String> {
    self.send_packet("s")
  }

  /// Send Ctrl-C and wait for a stop reply
  pub(super) fn interrupt(&mut self) -> Result<String> {
    self.stream.write_all(&[0x03])?;
    self.stream.flush()?;
    self.read_response_packet()
  }

  /// Insert a GDB remote breakpoint
  pub(super) fn insert_breakpoint(&mut self, kind: &str, addr: u64, size: u8) -> Result<String> {
    self.send_packet(&format!("{kind},{addr:x},{size:x}"))
  }

  /// Remove a GDB remote breakpoint
  pub(super) fn remove_breakpoint(&mut self, kind: &str, addr: u64, size: u8) -> Result<String> {
    self.send_packet(&format!("{kind},{addr:x},{size:x}"))
  }

  /// Read target memory through the remote protocol
  pub(super) fn read_memory(&mut self, addr: u64, len: usize) -> Result<Vec<u8>> {
    let response = self.send_packet(&format!("m{addr:x},{len:x}"))?;
    if response.starts_with('E') {
      bail!("remote target rejected memory read: {}", response);
    }
    hex_to_bytes(&response)
  }

  /// Write target memory through the remote protocol
  pub(super) fn write_memory(&mut self, addr: u64, bytes: &[u8]) -> Result<()> {
    let response = self.send_packet(&format!(
      "M{addr:x},{:x}:{}",
      bytes.len(),
      bytes_to_hex(bytes)
    ))?;
    expect_ok(&response)
  }

  /// Send a checksummed GDB remote packet and read the response
  pub(super) fn send_packet(&mut self, payload: &str) -> Result<String> {
    let encoded_packet = encode_packet(payload);
    trace_gdb_packet("send", payload);

    for _ in 0..3 {
      self.stream.write_all(encoded_packet.as_bytes())?;
      self.stream.flush()?;

      match self.read_byte()? {
        b'+' => return self.read_response_packet(),
        b'-' => {}
        b'$' => return self.read_response_packet_after_start(),
        other => bail!("unexpected GDB remote ack byte: 0x{other:02x}"),
      }
    }

    bail!("remote target rejected packet three times")
  }

  /// Read the next complete GDB remote packet
  pub(super) fn read_packet(&mut self) -> Result<String> {
    loop {
      if self.read_byte()? == b'$' {
        return self.read_packet_after_start();
      }
    }
  }

  /// Read packets until a real command response is received
  fn read_response_packet(&mut self) -> Result<String> {
    loop {
      let packet = self.read_packet()?;
      trace_gdb_packet("recv", &packet);
      if is_console_output_packet(&packet) {
        continue;
      }
      return Ok(packet);
    }
  }

  /// Read a response packet when the leading `$` has already been consumed
  fn read_response_packet_after_start(&mut self) -> Result<String> {
    let packet = self.read_packet_after_start()?;
    trace_gdb_packet("recv", &packet);
    if is_console_output_packet(&packet) {
      return self.read_response_packet();
    }
    Ok(packet)
  }

  /// Read packet bytes after the `$` marker and validate checksum
  fn read_packet_after_start(&mut self) -> Result<String> {
    let mut payload = Vec::new();
    loop {
      let byte = self.read_byte()?;
      if byte == b'#' {
        break;
      }
      payload.push(byte);
    }

    let mut checksum = [0u8; 2];
    self.stream.read_exact(&mut checksum)?;
    let expected = parse_hex_byte(checksum[0], checksum[1])?;
    let actual = packet_checksum(&payload);

    if actual != expected {
      self.stream.write_all(b"-")?;
      bail!(
        "bad GDB remote checksum: expected 0x{:02x}, got 0x{:02x}",
        expected,
        actual
      );
    }

    self.stream.write_all(b"+")?;
    self.stream.flush()?;
    Ok(String::from_utf8(payload)?)
  }

  /// Read one byte from the remote stream
  fn read_byte(&mut self) -> Result<u8> {
    let mut byte = [0u8; 1];
    if let Err(err) = self.stream.read_exact(&mut byte) {
      if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) {
        bail!("timed out waiting for GDB remote target response");
      }
      return Err(err).context("read from GDB remote target");
    }
    Ok(byte[0])
  }

  /// Run an operation with a temporary socket read timeout
  fn with_read_timeout<T>(
    &mut self,
    timeout: Option<Duration>,
    op: impl FnOnce(&mut Self) -> Result<T>,
  ) -> Result<T> {
    self.stream.set_read_timeout(timeout)?;
    let result = op(self);
    let restore_result = self.stream.set_read_timeout(Some(COMMAND_READ_TIMEOUT));

    match (result, restore_result) {
      (Ok(value), Ok(())) => Ok(value),
      (Err(err), Ok(())) => Err(err),
      (Ok(_), Err(err)) => Err(err.into()),
      (Err(err), Err(restore_err)) => Err(err.context(format!(
        "also failed to restore GDB remote read timeout: {restore_err}"
      ))),
    }
  }
}

/// Parse a hex string with or without a `0x` prefix
pub(super) fn parse_hex_u64(value: &str) -> Result<u64> {
  let hex = value.strip_prefix("0x").unwrap_or(value);
  u64::from_str_radix(hex, 16).map_err(|err| anyhow!("invalid hex value {value}: {err}"))
}

/// Require a GDB remote `OK` response
pub(super) fn expect_ok(response: &str) -> Result<()> {
  match response {
    "OK" => Ok(()),
    response if response.starts_with('E') => bail!("remote target returned error: {response}"),
    response => bail!("unexpected remote target response: {response}"),
  }
}

/// Format a remote stop reply for printing
pub(super) fn format_stop_reply(reply: &str) -> String {
  stop_reason_from_reply(reply).message()
}

/// Convert a remote stop reply into shared target state
pub(super) fn stop_reason_from_reply(reply: &str) -> StopReason {
  if let Some(code) = reply.strip_prefix('S').and_then(first_signal_byte) {
    return StopReason::Signal(format!("{} ({})", code, signal_name(code)));
  }
  if let Some(code) = reply.strip_prefix('T').and_then(first_signal_byte) {
    return StopReason::Signal(format!("{} ({})", code, signal_name(code)));
  }
  if let Some(code) = reply.strip_prefix('W').and_then(first_signal_byte) {
    return StopReason::Exited(i32::from(code));
  }
  if let Some(code) = reply.strip_prefix('X').and_then(first_signal_byte) {
    return StopReason::Terminated(format!("{} ({})", code, signal_name(code)));
  }

  StopReason::Other(format!("Remote stop reply: {reply}"))
}

/// Check whether a remote stop reply means the target exited
pub(super) fn stop_reply_is_exit(reply: &str) -> bool {
  reply.starts_with('W') || reply.starts_with('X')
}

/// Decode a hex string into bytes
pub(super) fn hex_to_bytes(hex: &str) -> Result<Vec<u8>> {
  let bytes = hex.as_bytes();
  if !bytes.len().is_multiple_of(2) {
    bail!("hex string has odd length");
  }

  let mut out = Vec::with_capacity(bytes.len() / 2);
  for pair in bytes.chunks_exact(2) {
    out.push(parse_hex_byte(pair[0], pair[1])?);
  }
  Ok(out)
}

/// Encode bytes as lowercase hexadecimal
pub(super) fn bytes_to_hex(bytes: &[u8]) -> String {
  bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Parse a little-endian u32 encoded as hex bytes
pub(super) fn parse_le_u32_hex(hex: &str) -> Result<u32> {
  let bytes = hex_to_bytes(hex)?;
  let Some(word) = bytes.get(..4) else {
    bail!("expected at least four bytes in register value");
  };
  Ok(u32::from_le_bytes(word.try_into()?))
}

/// Convert a u32 to little-endian bytes
pub(super) fn u32_to_le_bytes(value: u32) -> [u8; 4] {
  value.to_le_bytes()
}

/// Parse the first two hex characters as a signal number
fn first_signal_byte(value: &str) -> Option<u8> {
  let bytes = value.as_bytes();
  if bytes.len() < 2 {
    return None;
  }
  parse_hex_byte(bytes[0], bytes[1]).ok()
}

/// Map common signal numbers to names
fn signal_name(code: u8) -> &'static str {
  match code {
    2 => "SIGINT",
    3 => "SIGQUIT",
    5 => "SIGTRAP",
    6 => "SIGABRT",
    11 => "SIGSEGV",
    _ => "unknown",
  }
}

/// Wrap a payload in GDB remote packet framing
fn encode_packet(payload: &str) -> String {
  format!("${payload}#{:02x}", packet_checksum(payload.as_bytes()))
}

/// Compute the GDB remote checksum for a payload
fn packet_checksum(payload: &[u8]) -> u8 {
  payload
    .iter()
    .fold(0u8, |checksum, byte| checksum.wrapping_add(*byte))
}

/// Print GDB packet traces when explicitly requested for protocol debugging
fn trace_gdb_packet(direction: &str, payload: &str) {
  if std::env::var_os("RUST_DBG_GDB_TRACE").is_some() {
    eprintln!("gdb-remote {direction}: {payload}");
  }
}

/// `O` packets carry console output; `OK` is a normal success reply.
///
/// OpenOCD can emit a bare `O` packet with no payload, so treat it as console
/// output and keep waiting for the actual command response.
pub(super) fn is_console_output_packet(packet: &str) -> bool {
  let Some(output) = packet.strip_prefix('O') else {
    return false;
  };

  output.len() % 2 == 0 && output.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Parse two ASCII hex digits into one byte
fn parse_hex_byte(high: u8, low: u8) -> Result<u8> {
  Ok((hex_nibble(high)? << 4) | hex_nibble(low)?)
}

/// Parse one ASCII hex digit
fn hex_nibble(value: u8) -> Result<u8> {
  match value {
    b'0'..=b'9' => Ok(value - b'0'),
    b'a'..=b'f' => Ok(value - b'a' + 10),
    b'A'..=b'F' => Ok(value - b'A' + 10),
    _ => bail!("invalid hex digit: 0x{value:02x}"),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn encodes_gdb_packet_checksum() {
    assert_eq!(encode_packet("qSupported"), "$qSupported#37");
  }

  #[test]
  fn parses_little_endian_register_hex() {
    assert_eq!(parse_le_u32_hex("78563412").unwrap(), 0x12345678);
  }

  #[test]
  fn recognizes_bare_console_output_packet() {
    assert!(is_console_output_packet("O"));
    assert!(is_console_output_packet("O4869"));
    assert!(!is_console_output_packet("OK"));
  }
}
