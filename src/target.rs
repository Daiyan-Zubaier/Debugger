use anyhow::{Result, bail};

#[derive(Clone, Debug)]
pub struct RegisterValue {
  pub name: String,
  pub value: u64,
}

#[derive(Clone, Debug)]
pub struct BreakpointView {
  pub addr: u64,
  pub temporary: bool,
  pub enabled: bool,
}

#[derive(Clone, Debug)]
pub struct StackFrame {
  pub index: usize,
  pub pc: u64,
  pub location: String,
  pub approximate: bool,
}

#[derive(Clone, Debug)]
pub struct SourceLine {
  pub number: u64,
  pub text: String,
  pub is_current: bool,
}

#[derive(Clone, Debug)]
pub struct SourceView {
  pub path: Option<String>,
  pub current_line: Option<u64>,
  pub lines: Vec<SourceLine>,
}

#[derive(Clone, Debug)]
pub enum StopReason {
  Ready,
  Signal(String),
  Exited(i32),
  Terminated(String),
  Other(String),
}

impl StopReason {
  pub fn message(&self) -> String {
    match self {
      StopReason::Ready => "Target ready".to_string(),
      StopReason::Signal(sig) => format!("Stopped by {sig}"),
      StopReason::Exited(code) => format!("Exited with {code}"),
      StopReason::Terminated(sig) => format!("Terminated by {sig}"),
      StopReason::Other(msg) => msg.clone(),
    }
  }
}

#[derive(Clone, Debug)]
pub struct TargetSnapshot {
  pub name: String,
  pub architecture: String,
  pub pc: Option<u64>,
  pub location: String,
  pub source: SourceView,
  pub registers: Vec<RegisterValue>,
  pub breakpoints: Vec<BreakpointView>,
  pub backtrace: Vec<StackFrame>,
}

pub trait DebugTarget {
  fn name(&self) -> &str;
  fn architecture(&self) -> &str;

  fn initialize(&mut self) -> Result<StopReason>;
  fn continue_exec(&mut self) -> Result<StopReason>;
  fn step_instruction(&mut self) -> Result<StopReason>;
  fn halt(&mut self) -> Result<StopReason>;

  fn reset_halt(&mut self) -> Result<StopReason> {
    bail!("reset is not supported by this target")
  }

  fn pc(&mut self) -> Result<u64>;
  fn location(&self, pc: u64) -> String;
  fn current_source(&mut self, context_lines: usize) -> Result<SourceView>;

  fn registers(&mut self) -> Result<Vec<RegisterValue>>;
  fn write_register_by_name(&mut self, name: &str, value: u64) -> Result<()>;

  fn read_memory(&mut self, addr: u64, len: usize) -> Result<Vec<u8>>;
  fn write_memory(&mut self, addr: u64, data: &[u8]) -> Result<()>;

  fn set_breakpoint(&mut self, addr: u64) -> Result<()>;
  fn clear_breakpoint(&mut self, addr: u64) -> Result<()>;
  fn breakpoint_addr_for_source(&self, file: &str, line: u64) -> Result<Option<u64>>;
  fn breakpoints(&self) -> Vec<BreakpointView>;

  fn backtrace(&mut self) -> Result<Vec<StackFrame>>;

  // For the stdout produced by the program being debugged
  fn drain_output(&mut self) -> Result<Vec<u8>> {
    Ok(Vec::new())
  }

  fn step_source(&mut self) -> Result<StopReason> {
    let start_line = self.current_source(0)?.current_line;

    for _ in 0..4096 {
      let reason = self.step_instruction()?;
      if matches!(reason, StopReason::Exited(_) | StopReason::Terminated(_)) {
        return Ok(reason);
      }

      let current_line = self.current_source(0)?.current_line;
      if current_line != start_line {
        return Ok(reason);
      }
    }

    Ok(StopReason::Other("Step limit reached".to_string()))
  }

  fn next_source(&mut self) -> Result<StopReason> {
    self.step_source()
  }

  /// Produce a snapshot for the TUI
  fn snapshot(&mut self) -> Result<TargetSnapshot> {
    let pc = self.pc().ok();

    let location = pc
      .map(|pc| self.location(pc))
      .unwrap_or_else(|| "No program counter".to_string());

    let source = self
      .current_source(6)
      .unwrap_or_else(|_| empty_source_view());

    let registers = self.registers().unwrap_or_default();
    let breakpoints = self.breakpoints();
    let backtrace = self.backtrace().unwrap_or_default();

    Ok(TargetSnapshot {
      name: self.name().to_string(),
      architecture: self.architecture().to_string(),
      pc,
      location,
      source,
      registers,
      breakpoints,
      backtrace,
    })
  }
}

/// Shows a small slice of a source file around the current line (Used by the TUI)
pub fn read_source_window(
  path: &str,
  current_line: u64,
  context_lines: usize,
) -> Result<SourceView> {
  let contents = std::fs::read_to_string(path)?;
  let start_line = current_line.saturating_sub(context_lines as u64).max(1);
  let end_line = current_line.saturating_add(context_lines as u64);
  let mut lines = Vec::new();

  for (idx, text) in contents.lines().enumerate() {
    let number = idx as u64 + 1;
    if number < start_line || number > end_line {
      continue;
    }

    lines.push(SourceLine {
      number,
      text: text.to_string(),
      is_current: number == current_line,
    });
  }

  Ok(SourceView {
    path: Some(path.to_string()),
    current_line: Some(current_line),
    lines,
  })
}

pub fn empty_source_view() -> SourceView {
  SourceView {
    path: None,
    current_line: None,
    lines: vec![],
  }
}
