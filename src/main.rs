use std::env; 
use std::ffi::CString;
use nix::unistd::fork; 
use nix::unistd::ForkResult; 
use nix::unistd::write; 
use nix::sys::ptrace;

mod debugger; 
use debugger::Debugger; 

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 { 
        panic!("No program name specified"); 
    }
    
    let target_binary = &args[1]; 
    println!("Starting debugger...."); 

    /* Create a new child process (This will become our target program to debug) */
    match unsafe{fork()} {
        Ok(ForkResult::Parent { child, ..}) => { 
            println!("In parent process... child PID: {}, program_name: {}", child, target_binary.to_string()); 
            let mut dbg = Debugger::new(target_binary.to_string(), child);
            dbg.run();
        }
        Ok(ForkResult::Child) => {
            write(std::io::stdout(), "Starting debugging process....\n".as_bytes()).ok();
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
