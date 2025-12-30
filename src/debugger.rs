use nix::sys::signal::Signal::SIGCONT;
use nix::{unistd::Pid};
use nix::sys::wait::{waitpid, WaitStatus};
use rustyline::error::ReadlineError;
use rustyline::{DefaultEditor, Result};
use nix::sys::ptrace;

pub struct Debugger { 
  program_name: String,
  pid: Pid,
  is_executing: bool 
}

impl Debugger { 
  pub fn new(program_name: String, pid: Pid) -> Self { 
    Self { program_name, pid, is_executing: false}
  }

  pub fn run(&mut self) -> Result<()> { 
    /* 
     * For now Option is set to None. This means it only blocks until child exits or is killed. 
     * options is a bitmask that determines which state transitions to block 
     * Waits for thread to be ready
     */
    match waitpid(self.pid, None)? {
      /* Sends SIGTRAP signal */
      WaitStatus::Stopped(_, _) => { 
        println!("SIGTRAP received, ready to be debugged!"); 
      }
      _ => {
        println!("Unexpected status, returning......."); 
        return Ok(()); 
      }
    }
    
    /* 
     * Process is ready to be debugged, now let's start the command line input 
    */
    let mut rl = DefaultEditor::new()?;

    /* Checks if file history feature is enabled*/
    if rl.load_history("history.txt").is_err() {
      println!("No prev history"); 
    }
    loop {
      /* Ensure process is running  */
      if self.is_executing {
        match waitpid(self.pid, None)? {
          WaitStatus::Stopped(_, sig) => {
            println!("Stopped by {:?}", sig);
              self.is_executing = false;
          }
          WaitStatus::Exited(_, code) => {
            println!("Exited with {}", code);
            break;
          }
          WaitStatus::Signaled(_, sig, _) => {
            println!("Killed by {:?}", sig);
            break;
          }
          other => {
            println!("Other status: {:?}", other);
          }
        }
        continue;
      }
      let readline = rl.readline("(rust_dbg) "); 
      match readline { 
        Ok(line) => { 
          /* Pass in command to command handler */
          rl.add_history_entry(line.as_str())?; 
          self.handle_command(&line)?;
        }
        Err(ReadlineError::Interrupted) => {
          println!("CTRL-C");
          break
        },
        Err(ReadlineError::Eof) => {
          println!("CTRL-D");
          break
        },
        Err(err) => {
          println!("Error: {:?}", err);
          break
        }
      }
    }
    rl.save_history("history.txt")?;
    Ok(())
  }

  /*
   * For now the commands are: 
   * - continue (to continue execution of the program) 
   * - break (to set a breakpoint)
   * - and more...
   */
  fn handle_command(&mut self, line: &str) -> Result<()> { 
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let command = match tokens.first() {
        Some(cmd) => *cmd,
        None => return Ok(()), /* empty line, do nothing */ 
    };

    match command { 
      "continue" => {
        ptrace::cont(self.pid, SIGCONT)?; 
      }
      _ => { 
        println!("Invalid command"); 
      }
    }
    Ok(())

  } 

}