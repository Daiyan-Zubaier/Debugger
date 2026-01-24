use nix::libc::ADDR_NO_RANDOMIZE;
use nix::libc::personality;
use nix::sys::ptrace;
use nix::unistd::ForkResult;
use nix::unistd::fork;
use nix::unistd::write;
use std::env;
use std::ffi::CString;

mod breakpoint;
mod debugger;
mod elf_debug_info;
mod registers;

use debugger::Debugger;

fn main() {
  let args: Vec<String> = env::args().collect();
  if args.len() < 2 {
    panic!("No program name specified");
  }

  let target_binary = &args[1];
  println!("Starting debugger....");

  // Create a new child process (This will become our target program to debug)
  //TODO Cleanup code below
  match unsafe { fork() } {
    Ok(ForkResult::Parent { child, .. }) => {
      println!(
        "In parent process... child PID: {}, program_name: {}",
        child,
        target_binary.to_string()
      );
      let mut dbg = Debugger::new(target_binary.to_string(), child);
      dbg.run().unwrap();
    }
    Ok(ForkResult::Child) => {
      let new_personality = ADDR_NO_RANDOMIZE as nix::libc::c_ulong;
      unsafe {
        personality(new_personality);
      }
      write(
        std::io::stdout(),
        "Starting debugging process....\n".as_bytes(),
      )
      .ok();
      ptrace::traceme().expect("Failed to enable tracing");
      let program = CString::new(target_binary.as_str()).expect("CString creation failed");
      let args = [program.clone()];
      nix::unistd::execvp(&program, &args).expect("execvp failed");
    }
    Err(_) => {
      panic!("Fork failed!");
    }
  }
}
