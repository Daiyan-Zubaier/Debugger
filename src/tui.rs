use std::io;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
  EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::prelude::{Color, Line, Modifier, Span, Style};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::{Frame, Terminal};

use crate::target::{DebugTarget, SourceView, StopReason, TargetSnapshot};

const TUI_HISTORY_FILE: &str = "history.txt";

pub fn run_tui(target: &mut dyn DebugTarget) -> Result<()> {
  enable_raw_mode()?;

  let mut stdout = io::stdout();
  execute!(stdout, EnterAlternateScreen)?;

  let backend = CrosstermBackend::new(stdout); // To get stdout in ratatui
  let mut terminal = Terminal::new(backend)?;
  let result = TuiApp::new(target).run(&mut terminal);

  disable_raw_mode()?;
  execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
  terminal.show_cursor()?;

  result
}

struct TuiApp<'a> {
  target: &'a mut dyn DebugTarget,
  snapshot: Option<TargetSnapshot>,
  command: String,
  command_history: Vec<String>,
  history_index: Option<usize>,
  logs: Vec<String>,
  output_buffer: String,
  focused_pane: Pane,
  scroll: PaneScroll,
  should_quit: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Pane {
  Source,
  Registers,
  Breakpoints,
  Backtrace,
  Log,
}

#[derive(Default)]
struct PaneScroll {
  source: u16,
  registers: u16,
  breakpoints: u16,
  backtrace: u16,
  log: u16,
}

impl<'a> TuiApp<'a> {
  /// Build the in-memory TUI state around a target backend (ARM or linux)
  fn new(target: &'a mut dyn DebugTarget) -> Self {
    Self {
      target,
      snapshot: None,
      command: String::new(),
      command_history: load_tui_history(),
      history_index: None,
      logs: Vec::new(),
      output_buffer: String::new(),
      focused_pane: Pane::Source,
      scroll: PaneScroll::default(),
      should_quit: false,
    }
  }

  /// Run the terminal event loop until the user quits
  fn run(&mut self, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    self.log("Initializing target...");
    terminal.draw(|frame| self.draw(frame))?;

    let reason = self
      .target
      .initialize()
      .map_err(|err| anyhow!("target initialization failed: {err:#}"))?;
    self.log(reason.message());
    self.drain_target_output();
    terminal.draw(|frame| self.draw(frame))?;
    self.refresh_snapshot();

    while !self.should_quit {
      terminal.draw(|frame| self.draw(frame))?;

      if event::poll(Duration::from_millis(200))? {
        let Event::Key(key) = event::read()? else {
          continue;
        };
        self.handle_key(key)?;
      }
    }

    self.save_history();
    Ok(())
  }

  /// Draw the full TUI layout for the latest snapshot
  ///
  /// Split into 6 different areas for each pane
  /// ```text
  /// --------------------------------------------------------------------------------
  /// |                                  vertical[0]                                 |
  /// | |----------------------------------------------┬---------------------------| |
  /// | |                                              |        Registers          | |
  /// | |                                              |        side[0]            | |
  /// | |                                              |---------------------------| |
  /// | |                  Source                      |       Breakpoints         | |
  /// | |                  main[0]                     |       side[1]             | |
  /// | |                                              |---------------------------| |
  /// | |                                              |        Backtrace          | |
  /// | |                                              |        side[2]            | |
  /// | |----------------------------------------------|---------------------------| |
  /// |------------------------------------------------------------------------------|
  /// |                                Command input                                 |
  /// |                                vertical[1]                                   |
  /// |------------------------------------------------------------------------------|
  /// |                                  Logs                                        |
  /// |                                vertical[2]                                   |
  /// --------------------------------------------------------------------------------
  /// ```
  fn draw(&self, frame: &mut Frame) {
    let area = frame.area();
    let vertical = Layout::default()
      .direction(Direction::Vertical)
      .constraints([
        Constraint::Min(10),
        Constraint::Length(3),
        Constraint::Length(7),
      ])
      .split(area);

    let main = Layout::default()
      .direction(Direction::Horizontal)
      .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
      .split(vertical[0]);

    let side = Layout::default()
      .direction(Direction::Vertical)
      .constraints([
        Constraint::Percentage(48),
        Constraint::Percentage(22),
        Constraint::Percentage(30),
      ])
      .split(main[1]);

    self.draw_source(frame, main[0]);
    self.draw_registers(frame, side[0]);
    self.draw_breakpoints(frame, side[1]);
    self.draw_backtrace(frame, side[2]);
    self.draw_command(frame, vertical[1]);
    self.draw_logs(frame, vertical[2]);
  }

  /// Draw the source pane around the current program counter
  fn draw_source(&self, frame: &mut Frame, area: Rect) {
    frame.render_widget(Clear, area);

    let Some(snapshot) = &self.snapshot else {
      let widget = Paragraph::new("No target snapshot")
        .block(self.block(Pane::Source, "Source"))
        .wrap(Wrap { trim: false });
      frame.render_widget(widget, area);
      return;
    };

    let title = match (&snapshot.source.path, snapshot.pc) {
      (Some(path), Some(pc)) => format!("Source  {}  pc=0x{pc:x}", path),
      (Some(path), None) => format!("Source  {path}"),
      (None, Some(pc)) => format!("Source  pc=0x{pc:x}"),
      (None, None) => "Source".to_string(),
    };

    let lines = if snapshot.source.lines.is_empty() {
      missing_source_lines(snapshot)
    } else {
      source_lines(&snapshot.source)
    };

    let max_scroll = lines
      .len()
      .saturating_sub(area.height.saturating_sub(2) as usize) as u16;

    let scroll = self.scroll.source.min(max_scroll);

    let widget = Paragraph::new(lines)
      .block(self.block(Pane::Source, title))
      .scroll((scroll, 0))
      .wrap(Wrap { trim: false });

    frame.render_widget(widget, area);
  }

  /// Draw the current register snapshot
  fn draw_registers(&self, frame: &mut Frame, area: Rect) {
    frame.render_widget(Clear, area);

    let items = self
      .snapshot
      .as_ref()
      .map(|snapshot| {
        snapshot
          .registers
          .iter()
          .map(|reg| ListItem::new(format!("{:<8} 0x{:016x}", reg.name, reg.value)))
          .collect::<Vec<_>>()
      })
      .unwrap_or_default();

    let items = visible_items(items, self.scroll.registers, area);
    let widget = List::new(items).block(self.block(Pane::Registers, "Registers"));

    frame.render_widget(widget, area);
  }

  /// Draw the breakpoint list
  fn draw_breakpoints(&self, frame: &mut Frame, area: Rect) {
    frame.render_widget(Clear, area);
    let items = self
      .snapshot
      .as_ref()
      .map(|snapshot| {
        if snapshot.breakpoints.is_empty() {
          return vec![ListItem::new("No breakpoints")];
        }

        snapshot
          .breakpoints
          .iter()
          .map(|bp| {
            let marker = if bp.temporary { "temp" } else { "user" };
            let enabled = if bp.enabled { "enabled" } else { "disabled" };
            ListItem::new(format!("0x{:016x}  {marker}  {enabled}", bp.addr))
          })
          .collect::<Vec<_>>()
      })
      .unwrap_or_default();
    let items = visible_items(items, self.scroll.breakpoints, area);

    let widget = List::new(items).block(self.block(Pane::Breakpoints, "Breakpoints"));
    frame.render_widget(widget, area);
  }

  /// Draw the current backtrace
  fn draw_backtrace(&self, frame: &mut Frame, area: Rect) {
    frame.render_widget(Clear, area);
    let items = self
      .snapshot
      .as_ref()
      .map(|snapshot| {
        if snapshot.backtrace.is_empty() {
          return vec![ListItem::new("No backtrace")];
        }

        snapshot
          .backtrace
          .iter()
          .map(|frame| {
            let suffix = if frame.approximate { " approx" } else { "" };
            ListItem::new(format!(
              "#{:<2} 0x{:016x} {}{}",
              frame.index, frame.pc, frame.location, suffix
            ))
          })
          .collect::<Vec<_>>()
      })
      .unwrap_or_default();
    let items = visible_items(items, self.scroll.backtrace, area);

    let widget = List::new(items).block(self.block(Pane::Backtrace, "Backtrace"));
    frame.render_widget(widget, area);
  }

  /// Draw the command input pane
  fn draw_command(&self, frame: &mut Frame, area: Rect) {
    frame.render_widget(Clear, area);
    let prompt = Line::from(vec![
      Span::styled("rust_dbg> ", Style::default().fg(Color::Cyan)),
      Span::raw(&self.command),
    ]);
    let widget = Paragraph::new(prompt).block(
      Block::default()
        .title(
          "Command  F5=continue F10=next F11=stepi Ctrl+X=focus PgUp/PgDn=scroll Ctrl+P/N=history",
        )
        .borders(Borders::ALL),
    );
    frame.render_widget(widget, area);
    frame.set_cursor_position(command_cursor_position(area, &self.command));
  }

  /// Draw recent command and target messages
  fn draw_logs(&self, frame: &mut Frame, area: Rect) {
    frame.render_widget(Clear, area);
    let visible_height = area.height.saturating_sub(2) as usize;
    let max_start = self.logs.len().saturating_sub(visible_height);
    let start = max_start
      .saturating_sub(self.scroll.log as usize)
      .min(max_start);
    let items = self.logs[start..]
      .iter()
      .map(|log| ListItem::new(log.as_str()))
      .collect::<Vec<_>>();

    let widget = List::new(items).block(self.block(Pane::Log, "Log"));
    frame.render_widget(widget, area);
  }

  /// Build a pane block and mark the focused pane
  fn block(&self, pane: Pane, title: impl Into<String>) -> Block<'static> {
    let title = title.into();
    let block = Block::default().title(title).borders(Borders::ALL);
    if self.focused_pane == pane {
      block.border_style(Style::default().fg(Color::Yellow))
    } else {
      block
    }
  }

  /// Convert key presses into TUI commands
  ///
  /// # Commands
  /// - Ctrl + x - Switch panes
  /// - Ctrl + c - halt execution for the target program
  /// - Ctrl + q - Quit the debugger
  /// - Ctrl + p - Scroll up in command history
  /// - Ctrl + q - Scroll down in command history
  /// - F5       - Same as running `continue` in the command window
  /// - F10      - Same as running `next` in the command window
  /// - F11      - Same as running `stepi` in the command window
  /// - PageUp   - Scroll up in the highlighted pane
  /// - PageDown - Scroll down in the highligted pane
  /// - Escape   - Command window specific: Lets you clear your currently typed command
  fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
    match key.code {
      KeyCode::Char('x') if key.modifiers.contains(KeyModifiers::CONTROL) => {
        self.focused_pane = self.focused_pane.next();
      }
      KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
        self.run_target_op("halt", |target| target.halt())?;
      }
      KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => {
        self.should_quit = true
      }
      KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
        self.history_previous();
      }
      KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
        self.history_next();
      }
      KeyCode::F(5) => self.run_target_op("continue", |target| target.continue_exec())?,
      KeyCode::F(10) => self.run_target_op("next", |target| target.next_source())?,
      KeyCode::F(11) => self.run_target_op("stepi", |target| target.step_instruction())?,
      KeyCode::PageUp => self.scroll_focused(-10),
      KeyCode::PageDown => self.scroll_focused(10),
      KeyCode::Up => self.scroll_focused(-1),
      KeyCode::Down => self.scroll_focused(1),
      KeyCode::Enter => {
        let command = self.command.trim().to_string();
        self.command.clear();
        self.history_index = None;
        if !command.is_empty() {
          self.push_history(command.clone());
          if let Err(err) = self.execute_command(&command) {
            self.log(format!("command failed: {err:#}"));
            self.refresh_snapshot();
          }
        }
      }
      KeyCode::Backspace => {
        self.command.pop();
        self.history_index = None;
      }
      KeyCode::Esc => {
        self.command.clear();
        self.history_index = None;
      }
      KeyCode::Char(ch) => {
        self.command.push(ch);
        self.history_index = None;
      }
      _ => {}
    }

    Ok(())
  }

  /// Scroll the currently focused pane
  fn scroll_focused(&mut self, delta: i16) {
    let value = self.focused_scroll_mut();
    if delta.is_negative() {
      *value = value.saturating_sub(delta.unsigned_abs());
    } else {
      *value = value.saturating_add(delta as u16);
    }
  }

  /// Return a mutable scroll slot for the focused pane
  fn focused_scroll_mut(&mut self) -> &mut u16 {
    match self.focused_pane {
      Pane::Source => &mut self.scroll.source,
      Pane::Registers => &mut self.scroll.registers,
      Pane::Breakpoints => &mut self.scroll.breakpoints,
      Pane::Backtrace => &mut self.scroll.backtrace,
      Pane::Log => &mut self.scroll.log,
    }
  }

  /// Add a command to history without duplicating adjacent entries
  fn push_history(&mut self, command: String) {
    if self.command_history.last() != Some(&command) {
      self.command_history.push(command);
    }
  }

  /// Move to the previous command in history
  fn history_previous(&mut self) {
    if self.command_history.is_empty() {
      return;
    }

    let index = self
      .history_index
      .map(|index| index.saturating_sub(1))
      .unwrap_or_else(|| self.command_history.len() - 1);
    self.history_index = Some(index);
    self.command = self.command_history[index].clone();
  }

  /// Move to the next command in history
  fn history_next(&mut self) {
    let Some(index) = self.history_index else {
      return;
    };

    let next_index = index + 1;
    if next_index >= self.command_history.len() {
      self.history_index = None;
      self.command.clear();
      return;
    }

    self.history_index = Some(next_index);
    self.command = self.command_history[next_index].clone();
  }

  /// Parse and execute a command typed into the TUI command line
  ///
  /// # Commands
  /// - continue         - run until the target stops again
  /// - stepi, si        - single-step one machine instruction
  /// - step, s          - step until the source line changes, entering calls
  /// - next, n          - step until the source line changes, stepping over calls
  /// - halt             - request that the target stops
  /// - reset            - reset and halt the target, when supported
  /// - break, b         - set an address or source-level breakpoint
  /// - clear, delete    - remove a breakpoint
  /// - register, reg    - inspect or update registers
  /// - memory, mem      - read or write target memory
  /// - backtrace, bt    - refresh stack frames
  /// - help, h          - print command help
  /// - quit, q          - exit the TUI
  fn execute_command(&mut self, line: &str) -> Result<()> {
    let tokens = line.split_whitespace().collect::<Vec<_>>();
    let Some(command) = tokens.first().copied() else {
      return Ok(());
    };

    self.log(format!("> {line}"));
    match command {
      "continue" | "c" => self.run_target_op("continue", |target| target.continue_exec())?,
      "stepi" | "si" => self.run_target_op("stepi", |target| target.step_instruction())?,
      "step" | "s" => self.run_target_op("step", |target| target.step_source())?,
      "next" | "n" => self.run_target_op("next", |target| target.next_source())?,
      "halt" => self.run_target_op("halt", |target| target.halt())?,
      "reset" => self.run_target_op("reset", |target| target.reset_halt())?,
      "break" | "b" => self.cmd_break(&tokens)?,
      "clear" | "delete" => self.cmd_clear(&tokens)?,
      "register" | "reg" => self.cmd_register(&tokens)?,
      "memory" | "mem" => self.cmd_memory(&tokens)?,
      "backtrace" | "bt" => self.refresh_snapshot(),
      "help" | "h" => self.print_help(),
      "quit" | "q" => self.should_quit = true,
      _ => self.log(format!("Invalid command: {command}")),
    }

    Ok(())
  }

  /// Run a target operation and refresh the visible state afterward
  fn run_target_op<F>(&mut self, label: &str, mut op: F) -> Result<()>
  where
    F: FnMut(&mut dyn DebugTarget) -> Result<StopReason>,
  {
    self.log(format!("running {label}..."));
    match op(self.target) {
      Ok(reason) => self.log(reason.message()),
      Err(err) => self.log(format!("{label} failed: {err}")),
    }
    self.drain_target_output();
    self.refresh_snapshot();
    Ok(())
  }

  /// Set an address or source-level breakpoint from TUI command tokens
  fn cmd_break(&mut self, tokens: &[&str]) -> Result<()> {
    let Some(arg) = tokens.get(1) else {
      self.log("Usage: break 0x<addr> OR break <file>:<line>");
      return Ok(());
    };

    let addr = if !arg.starts_with("0x") && arg.contains(':') {
      let (file, line) = parse_source_location(arg)?;
      let Some(addr) = self.target.breakpoint_addr_for_source(file, line)? else {
        self.log(format!("No code found at {file}:{line}"));
        return Ok(());
      };
      addr
    } else {
      parse_hex_u64(arg)?
    };

    if let Err(err) = self.target.set_breakpoint(addr) {
      if is_remote_timeout(&err) {
        self.log("Breakpoint timed out; halting target and retrying...");
        match self.target.halt() {
          Ok(reason) => self.log(reason.message()),
          Err(halt_err) => {
            return Err(halt_err.context(format!(
              "breakpoint set timed out at 0x{addr:x}; halt before retry also failed"
            )));
          }
        }
        self.target.set_breakpoint(addr)?;
      } else {
        return Err(err);
      }
    }
    self.log(format!("Breakpoint set at 0x{addr:x}"));
    self.refresh_snapshot();
    Ok(())
  }

  /// Clear a breakpoint from TUI command tokens
  fn cmd_clear(&mut self, tokens: &[&str]) -> Result<()> {
    let Some(arg) = tokens.get(1) else {
      self.log("Usage: clear 0x<addr>");
      return Ok(());
    };

    let addr = parse_hex_u64(arg)?;
    self.target.clear_breakpoint(addr)?;
    self.log(format!("Breakpoint cleared at 0x{addr:x}"));
    self.refresh_snapshot();
    Ok(())
  }

  /// Handle TUI register commands
  fn cmd_register(&mut self, tokens: &[&str]) -> Result<()> {
    match tokens.get(1).copied() {
      Some("dump") | None => self.refresh_snapshot(),
      Some("read") => {
        let Some(name) = tokens.get(2) else {
          self.log("Usage: register read <name>");
          return Ok(());
        };
        let registers = self.target.registers()?;
        match registers.iter().find(|reg| reg.name == *name) {
          Some(reg) => self.log(format!("{} = 0x{:x}", reg.name, reg.value)),
          None => self.log(format!("Unknown register: {name}")),
        }
      }
      Some("write") => {
        let Some(name) = tokens.get(2) else {
          self.log("Usage: register write <name> 0x<value>");
          return Ok(());
        };
        let Some(value) = tokens.get(3) else {
          self.log("Usage: register write <name> 0x<value>");
          return Ok(());
        };
        let value = parse_hex_u64(value)?;
        self.target.write_register_by_name(name, value)?;
        self.log(format!("{name} = 0x{value:x}"));
        self.refresh_snapshot();
      }
      Some(other) => self.log(format!("Unknown register subcommand: {other}")),
    }

    Ok(())
  }

  /// Handle TUI memory read and write commands
  fn cmd_memory(&mut self, tokens: &[&str]) -> Result<()> {
    let Some(subcmd) = tokens.get(1).copied() else {
      self.log("Usage: memory read 0x<addr> [len] OR memory write 0x<addr> 0x<value>");
      return Ok(());
    };
    let Some(addr) = tokens.get(2) else {
      self.log("Usage: memory read 0x<addr> [len] OR memory write 0x<addr> 0x<value>");
      return Ok(());
    };
    let addr = parse_hex_u64(addr)?;

    match subcmd {
      "read" => {
        let len = tokens
          .get(3)
          .and_then(|value| value.parse::<usize>().ok())
          .unwrap_or(16);
        let bytes = self.target.read_memory(addr, len)?;
        for line in hexdump(addr, &bytes) {
          self.log(line);
        }
      }
      "write" => {
        let Some(value) = tokens.get(3) else {
          self.log("Usage: memory write 0x<addr> 0x<value>");
          return Ok(());
        };
        let bytes = (parse_hex_u64(value)? as u32).to_le_bytes();
        self.target.write_memory(addr, &bytes)?;
        self.log(format!("Wrote 0x{} to 0x{addr:x}", bytes_to_hex(&bytes)));
        self.refresh_snapshot();
      }
      other => self.log(format!("Unknown memory subcommand: {other}")),
    }

    Ok(())
  }

  /// Show a compact command summary in the log pane
  fn print_help(&mut self) {
    self.log("Commands: c, s, n, si, b, clear, reg, mem, bt, halt, reset, q");
    self.log("Function keys: F5 continue, F10 next, F11 instruction step");
  }

  /// Pull a fresh snapshot from the target backend
  fn refresh_snapshot(&mut self) {
    match self.target.snapshot() {
      Ok(snapshot) => self.snapshot = Some(snapshot),
      Err(err) => self.log(format!("snapshot failed: {err}")),
    }
  }

  /// Append any captured debuggee stdout/stderr to the log pane
  fn drain_target_output(&mut self) {
    match self.target.drain_output() {
      Ok(bytes) if !bytes.is_empty() => {
        let text = String::from_utf8_lossy(&bytes);
        self.output_buffer.push_str(&text);

        while let Some(newline_index) = self.output_buffer.find('\n') {
          let mut line = self
            .output_buffer
            .drain(..=newline_index)
            .collect::<String>();
          line = line.trim_end_matches(['\r', '\n']).to_string();
          self.log(format!("target> {line}"));
        }
      }
      Ok(_) => {}
      Err(err) => self.log(format!("target output read failed: {err}")),
    }
  }

  /// Append a bounded log message
  fn log(&mut self, msg: impl Into<String>) {
    self.logs.push(msg.into());
    if self.logs.len() > 500 {
      let extra = self.logs.len() - 500;
      self.logs.drain(0..extra);
    }
  }

  /// Persist command history for the next TUI session
  fn save_history(&self) {
    let start = self.command_history.len().saturating_sub(500);
    let contents = self.command_history[start..].join("\n");
    let _ = std::fs::write(TUI_HISTORY_FILE, format!("{contents}\n"));
  }
}

impl Pane {
  fn next(self) -> Self {
    match self {
      Pane::Source => Pane::Registers,
      Pane::Registers => Pane::Breakpoints,
      Pane::Breakpoints => Pane::Backtrace,
      Pane::Backtrace => Pane::Log,
      Pane::Log => Pane::Source,
    }
  }
}

/// Load command history shared with the non-tui debugger
fn load_tui_history() -> Vec<String> {
  std::fs::read_to_string(TUI_HISTORY_FILE)
    .map(|contents| {
      contents
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(ToString::to_string)
        .collect()
    })
    .unwrap_or_default()
}

/// Slice list items to the visible scroll window for a pane
fn visible_items(items: Vec<ListItem<'_>>, offset: u16, area: Rect) -> Vec<ListItem<'_>> {
  let visible_height = area.height.saturating_sub(2) as usize;
  if visible_height == 0 || items.is_empty() {
    return Vec::new();
  }

  let max_start = items.len().saturating_sub(visible_height);
  let start = (offset as usize).min(max_start);
  items.into_iter().skip(start).take(visible_height).collect()
}

/// Convert source rows into styled terminal lines
fn source_lines(source: &SourceView) -> Vec<Line<'static>> {
  source
    .lines
    .iter()
    .map(|line| {
      let marker = if line.is_current { ">" } else { " " };
      let text = format!("{marker} {:>5}  {}", line.number, line.text);
      let style = if line.is_current {
        Style::default()
          .fg(Color::Yellow)
          .add_modifier(Modifier::BOLD)
      } else {
        Style::default()
      };
      Line::from(Span::styled(text, style))
    })
    .collect()
}

/// Explain why the source pane cannot show file-backed source for the current PC
fn missing_source_lines(snapshot: &TargetSnapshot) -> Vec<Line<'static>> {
  vec![
    Line::raw("No source line available for the current PC."),
    Line::raw(format!("Location: {}", snapshot.location)),
    Line::raw("Continue or step until execution reaches code built with debug info."),
  ]
}

/// Place the visible terminal cursor at the end of the command input
fn command_cursor_position(area: Rect, command: &str) -> Position {
  let prompt_width = "rust_dbg> ".len() as u16;
  let command_width = command.chars().count() as u16;
  let inner_width = area.width.saturating_sub(2);
  let x_offset = prompt_width
    .saturating_add(command_width)
    .min(inner_width.saturating_sub(1));

  Position {
    x: area.x.saturating_add(1).saturating_add(x_offset),
    y: area.y.saturating_add(1),
  }
}

/// Parse a `<file>:<line>` source breakpoint location
fn parse_source_location(value: &str) -> Result<(&str, u64)> {
  let Some((file, line)) = value.rsplit_once(':') else {
    bail!("expected <file>:<line>");
  };
  let line = line.parse::<u64>()?;
  Ok((file, line))
}

/// Parse a hex string with or without a `0x` prefix
fn parse_hex_u64(value: &str) -> Result<u64> {
  let hex = value.strip_prefix("0x").unwrap_or(value);
  u64::from_str_radix(hex, 16).map_err(|err| anyhow!("invalid hex value {value}: {err}"))
}

/// Format bytes as a simple hex dump
fn hexdump(base_addr: u64, bytes: &[u8]) -> Vec<String> {
  bytes
    .chunks(16)
    .enumerate()
    .map(|(line_index, chunk)| {
      let bytes = chunk
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ");
      format!("{:08x}: {}", base_addr + (line_index * 16) as u64, bytes)
    })
    .collect()
}

/// Encode bytes as lowercase hexadecimal
fn bytes_to_hex(bytes: &[u8]) -> String {
  bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Detect timeout errors from the synchronous GDB remote path
fn is_remote_timeout(err: &anyhow::Error) -> bool {
  err.chain().any(|cause| {
    let text = cause.to_string();
    text.contains("timed out waiting for GDB remote target response")
      || text.contains("Resource temporarily unavailable")
  })
}
