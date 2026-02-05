use std::env;
use std::ffi::CString;
use std::process;

use nix::libc::{ADDR_NO_RANDOMIZE, personality};
use nix::sys::ptrace;
use nix::unistd::{ForkResult, fork};

use linux_debugger::Debugger;

fn main() {
  let args: Vec<String> = env::args().collect();
  if args.len() < 2 {
    eprintln!("Usage: {} <program>", args[0]);
    process::exit(1);
  }

  let target_binary = &args[1];
  println!("Starting debugger for '{}'", target_binary);

  // Fork: child becomes the debuggee, parent becomes the debugger
  match unsafe { fork() } {
    Ok(ForkResult::Parent { child, .. }) => {
      // Parent process - run the debugger
      let mut dbg = Debugger::new(target_binary.to_string(), child);
      if let Err(e) = dbg.run() {
        eprintln!("Debugger error: {}", e);
        process::exit(1);
      }
    }
    Ok(ForkResult::Child) => {
      // Child process - become the target program
      run_target(target_binary);
    }
    Err(e) => {
      eprintln!("Fork failed: {}", e);
      process::exit(1);
    }
  }
}

/// Set up and execute the target program to be debugged
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
  nix::unistd::execvp(&program, &args).expect("execvp failed");

  unreachable!("Achievement: How did we get here?")
}
