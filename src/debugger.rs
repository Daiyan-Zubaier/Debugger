use std::collections::HashMap;
use std::ops::Add;

use nix::Result;
use nix::libc::c_long;
use nix::sys::signal::Signal::SIGCONT;
use nix::{unistd::Pid};
use nix::sys::wait::{waitpid, WaitStatus};
use rustyline::error::ReadlineError;
use rustyline::{DefaultEditor};
use nix::sys::ptrace::{self, AddressType};

use crate::breakpoint::{Breakpoint}; 
use crate::registers::{REG_DESCS, Register, get_reg_val_from_dwarf, get_register_from_name, get_register_name, get_register_value, set_register_value};

pub struct Debugger { 
  program_name: String,
  pid: Pid,
  is_executing: bool, 
  breakpoints: HashMap<AddressType, Breakpoint>
}

impl Debugger { 
  pub fn new(program_name: String, pid: Pid) -> Self { 
    Self { program_name, pid, is_executing: false, breakpoints: HashMap::new()}
  }

  pub fn run(&mut self) -> rustyline::Result<()> { 
    /* 
     * For now Option is set to None. This means it only blocks until child exits or is killed. 
     * options is a bitmask that determines which state transitions to block 
     * Waits for thread to be ready
     */
    match waitpid(self.pid, None)? {
      /* Sends SIGTRAP signal */
      WaitStatus::Stopped(_, _) => { 
        println!("SIGTRAP received, {} ready to be debugged!", self.program_name); 
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
            println!("PC = 0x{:x}", self.get_pc()?);
            // Only step over breakpoint if we stopped due to SIGTRAP
            if sig == nix::sys::signal::Signal::SIGTRAP {
              self.stepover_breakpoint()?;
            }
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
          // Pass in command to command handler
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
        self.is_executing = true;
      }
      "break" => {
        let addr_val = usize::from_str_radix(tokens[1].strip_prefix("0x").unwrap(), 16).unwrap();
        let bp_addr = addr_val as AddressType;
        
        
        let mut bp = Breakpoint::new(self.pid, bp_addr);
        match bp.enable() {
          Ok(_) => {
            println!("Breakpoint set at 0x{:x}", addr_val);
          }
          Err(e) => println!("Failed to set breakpoint: {:?}", e),
        }
        self.breakpoints.insert(bp_addr, bp); 
      }
      "register" => {
        match tokens[1] {
          "dump" => {
            for reg_desc in REG_DESCS.iter() {
              let value = get_register_value(self.pid, reg_desc.reg)?;
              println!("{:<10}  {:0>16x}", reg_desc.name, value);
            }
          }
          /*
           * Assumed command:
           * register read <reg_name>  
           */
          "read" => {
            let register_name = get_register_from_name(tokens[2]); 
            match register_name { 
              Some(reg_name) => println!("{}", get_register_value(self.pid, reg_name)?), 
              None => println!("Invalid register_name...."),  
            }
          }
          /*
           * Assumed command: 
           * register write <reg_name> 0x<Value> 
           */
          "write" => {
            let value_string = tokens[3].strip_prefix("0x").unwrap_or(tokens[3]);
            let value = match u64::from_str_radix(value_string, 16) {
              Ok(v) => v,
              Err(_) => {
                println!("Invalid hex value: {}", value_string);
                return Ok(());
              }
            }; // let value 
            let reg_name = get_register_from_name(tokens[2]); 
            match reg_name { 
              Some(reg) => set_register_value(self.pid, reg, value)?,
              None => println!("Invalid register name..."), 
            }
          }
          _ => {
            println!("Command not found"); 
          }
        }
      }
      "memory" => { 
        let addr_val = usize::from_str_radix(tokens[2].strip_prefix("0x").unwrap(), 16).unwrap();
        let addr = addr_val as AddressType;
        match tokens[1] {
          "read" => {
            println!("{}", ptrace::read(self.pid, addr)?); 
          } 
          "write" => {
            let data = c_long::from_str_radix(tokens[2].strip_prefix("0x").unwrap(), 16).unwrap();
            ptrace::write(self.pid, addr, data)?; 
            println!("Data {} was written to address {}", data, addr_val);
          }
          _ => {
            println!("Invalid memory command");
          }
        }
      }
      _ => { 
        println!("Invalid command"); 
      }
    }
    Ok(())
  } 

  fn stepover_breakpoint(&mut self) -> Result<()> { 
    let possible_bp_location = (self.get_pc()? - 1) as AddressType; 
    
    // First check if breakpoint exists and disable it
    if let Some(bp) = self.breakpoints.get_mut(&possible_bp_location) {
      if bp.enabled_status {
        bp.disable()?;
      } else {
        return Ok(()); // Not enabled, nothing to do
      }
    } else {
      return Ok(()); // No breakpoint at this location
    }
    // Mutable borrow of self.breakpoints ends here when bp goes out of scope

    let prev_instruction_address = possible_bp_location as u64; 
    self.set_pc(prev_instruction_address)?; 
    ptrace::step(self.pid, None)?;
    self.wait_for_signal()?;
    
    // Re-borrow mutably to re-enable the breakpoint
    if let Some(bp) = self.breakpoints.get_mut(&possible_bp_location) {
      bp.enable()?;
    }
    
    Ok(())
  }
  
  fn wait_for_signal(&self) -> Result<WaitStatus> {
    waitpid(self.pid, None)
  }
  
  fn get_pc(&self) -> Result<u64> { 
    get_register_value(self.pid, Register::Rip)
  }
  fn set_pc(&self, value: u64) -> Result<()> { 
    set_register_value(self.pid, Register::Rip, value)
  }

}