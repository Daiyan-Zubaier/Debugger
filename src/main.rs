use std::env;
use std::ffi::CString;
use std::os::fd::RawFd;
use std::process;

use nix::libc::{
  ADDR_NO_RANDOMIZE, F_GETFL, F_SETFL, O_NONBLOCK, STDERR_FILENO, STDOUT_FILENO, fcntl, openpty,
  personality,
};
use nix::sys::ptrace;
use nix::unistd::{ForkResult, fork};

use linux_debugger::tui::run_tui;
use linux_debugger::{LinuxPtraceDebugger, RemoteArmDebugger};

//TODO Debulk main.rs maybe a utils.rs?

/// Parse CLI flags and launch the selected debugger mode
fn main() {
  let args: Vec<String> = env::args().collect();
  let program_name = args[0].clone();
  let mut debugger_args = args[1..].to_vec();

  if debugger_args.is_empty() {
    print_usage(&args[0]);
    process::exit(1);
  }

  if debugger_args
    .iter()
    .any(|arg| arg == "--help" || arg == "-h")
  {
    print_usage(&program_name);
    return;
  }

  let use_tui = debugger_args.iter().any(|arg| arg == "--tui");
  debugger_args.retain(|arg| arg != "--tui");

  if debugger_args.is_empty() {
    print_usage(&program_name);
    process::exit(1);
  }

  if debugger_args[0] == "--arm-gdb" || debugger_args[0] == "--gdb-remote" {
    run_remote_arm_debugger(&program_name, &debugger_args, use_tui);
    return;
  }

  run_local_linux_debugger(&debugger_args[0], use_tui);
}

/// Fork and run the local Linux debugger path
fn run_local_linux_debugger(target_binary: &str, use_tui: bool) {
  println!("Starting debugger for '{target_binary}'");

  let output_pty = if use_tui {
    match create_output_pty() {
      Ok(pty) => Some(pty),
      Err(err) => {
        eprintln!("Failed to create TUI output capture PTY: {err}");
        process::exit(1);
      }
    }
  } else {
    None
  };

  // Fork: child becomes the debuggee, parent becomes the debugger
  match unsafe { fork() } {
    Ok(ForkResult::Parent { child, .. }) => {
      let output_fd = output_pty.map(|(master_fd, slave_fd)| {
        unsafe {
          nix::libc::close(slave_fd);
        }
        master_fd
      });

      // Parent process - run the debugger
      let mut dbg =
        LinuxPtraceDebugger::new_with_output_fd(target_binary.to_string(), child, output_fd);

      let result = if use_tui {
        run_tui(&mut dbg).map_err(|err| anyhow_to_string(&err))
      } else {
        dbg.run().map_err(|err| format!("{err:?}"))
      };

      // We should not get here
      if let Err(e) = result {
        eprintln!("LinuxPtraceDebugger error: {e}");
        process::exit(1);
      }
    }
    Ok(ForkResult::Child) => {
      if let Some((master_fd, slave_fd)) = output_pty {
        setup_captured_stdio(master_fd, slave_fd);
      }

      // Child process - become the target program
      run_target(target_binary);
    }
    Err(e) => {
      eprintln!("Fork failed: {e}");
      process::exit(1);
    }
  }
}

/// Create a nonblocking pseudo-terminal used to capture TUI debuggee output
fn create_output_pty() -> std::io::Result<(RawFd, RawFd)> {
  let mut master_fd = 0;
  let mut slave_fd = 0;
  let rc = unsafe {
    openpty(
      &mut master_fd,
      &mut slave_fd,
      std::ptr::null_mut(),
      std::ptr::null(),
      std::ptr::null(),
    )
  };

  if rc == -1 {
    return Err(std::io::Error::last_os_error());
  }

  set_nonblocking(master_fd)?;
  Ok((master_fd, slave_fd))
}

/// Mark a file descriptor as nonblocking
fn set_nonblocking(fd: RawFd) -> std::io::Result<()> {
  let flags = unsafe { fcntl(fd, F_GETFL) };
  if flags == -1 {
    return Err(std::io::Error::last_os_error());
  }

  let rc = unsafe { fcntl(fd, F_SETFL, flags | O_NONBLOCK) };
  if rc == -1 {
    return Err(std::io::Error::last_os_error());
  }

  Ok(())
}

/// Connect the child process stdout/stderr to the PTY slave
fn setup_captured_stdio(master_fd: RawFd, slave_fd: RawFd) {
  unsafe {
    nix::libc::close(master_fd);
    if nix::libc::dup2(slave_fd, STDOUT_FILENO) == -1 {
      eprintln!("dup2 stdout failed: {}", std::io::Error::last_os_error());
      process::exit(1);
    }
    if nix::libc::dup2(slave_fd, STDERR_FILENO) == -1 {
      eprintln!("dup2 stderr failed: {}", std::io::Error::last_os_error());
      process::exit(1);
    }
    nix::libc::close(slave_fd);
  }
}

/// Print supported command-line modes
fn print_usage(program_name: &str) {
  eprintln!("Usage:");
  eprintln!("  {program_name} <linux-program>");
  eprintln!("  {program_name} --tui <linux-program>");
  eprintln!("  {program_name} --arm-gdb <firmware.elf> [host:port]");
  eprintln!("  {program_name} --tui --arm-gdb <firmware.elf> [host:port]");
  eprintln!("  {program_name} --gdb-remote <firmware.elf> [host:port]");
}

/// Connect to an ARM GDB remote target and run the selected frontend
fn run_remote_arm_debugger(program_name: &str, args: &[String], use_tui: bool) {
  if args.len() < 2 {
    print_usage(program_name);
    process::exit(1);
  }

  let firmware_elf = args[1].clone();
  let endpoint = args.get(2).cloned();

  let mut dbg = match RemoteArmDebugger::connect(firmware_elf, endpoint) {
    Ok(dbg) => dbg,
    Err(e) => {
      eprintln!("Failed to connect to ARM target: {e}");
      process::exit(1);
    }
  };

  let result = if use_tui {
    run_tui(&mut dbg).map_err(|err| anyhow_to_string(&err))
  } else {
    dbg.run().map_err(|err| anyhow_to_string(&err))
  };

  if let Err(e) = result {
    eprintln!("LinuxPtraceDebugger error: {e}");
    process::exit(1);
  }
}

/// Render an anyhow error with its context chain
fn anyhow_to_string(err: &anyhow::Error) -> String {
  format!("{err:#}")
}

/// Set up and execute the target program to be debugged
/// Prepare the child process and exec the Linux debuggee
fn run_target(target_binary: &str) -> ! {
  // Disable ASLR for consistent addresses during debugging
  unsafe {
    personality(ADDR_NO_RANDOMIZE as nix::libc::c_ulong);
  }

  // Allow parent to trace this process
  ptrace::traceme().expect("Failed to enable tracing");

  // Execute the target program
  let program = CString::new(target_binary).expect("Invalid program path");
  let args = [program.clone()];
  match nix::unistd::execvp(&program, &args) {
    Ok(infallible) => match infallible {},
    Err(err) => panic!("execvp failed: {err}"),
  }
}
