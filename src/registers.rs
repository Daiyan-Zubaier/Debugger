
//TODO:  Add ARM support
//TODO:  Clean up match statements to maybe use REG_DESCS?
//!
use nix::Result;
// use nix::libc::user_regs_struct;
use nix::unistd::Pid;
use nix::sys::ptrace;

use strum::EnumCount;  // To derive a count of registers

#[derive(EnumCount, Clone, Copy)]
pub enum Register {
	// General-purpose registers
	Rax, Rdx, Rcx, Rbx,
	Rsi, Rdi, Rbp, Rsp,

	// Extended registers
	R8,  R9,  R10, R11,
	R12, R13, R14, R15,

	// Control registers
	Rip,
	RFlags,

	// Segment registers
	Es, Cs, Ss, Ds, Fs, Gs,

	// Segment bases
	FsBase,
	GsBase,

	// Syscall ABI
	OrigRax,
}

const NUMBER_OF_REGISTERS: usize = Register::COUNT;

pub struct RegDesc { 
  pub reg: Register, 
  pub dwarf: Option<u16>, 
  pub name: &'static str
}

pub const REG_DESCS: [RegDesc; NUMBER_OF_REGISTERS] = [
  // General purpose registers
  RegDesc { reg: Register::Rax,     dwarf: Some(0),  name: "rax" },
  RegDesc { reg: Register::Rdx,     dwarf: Some(1),  name: "rdx" },
  RegDesc { reg: Register::Rcx,     dwarf: Some(2),  name: "rcx" },
  RegDesc { reg: Register::Rbx,     dwarf: Some(3),  name: "rbx" },

  RegDesc { reg: Register::Rsi,     dwarf: Some(4),  name: "rsi" },
  RegDesc { reg: Register::Rdi,     dwarf: Some(5),  name: "rdi" },
  RegDesc { reg: Register::Rbp,     dwarf: Some(6),  name: "rbp" },
  RegDesc { reg: Register::Rsp,     dwarf: Some(7),  name: "rsp" },

  RegDesc { reg: Register::R8,      dwarf: Some(8),  name: "r8"  },
  RegDesc { reg: Register::R9,      dwarf: Some(9),  name: "r9"  },
  RegDesc { reg: Register::R10,     dwarf: Some(10), name: "r10" },
  RegDesc { reg: Register::R11,     dwarf: Some(11), name: "r11" },

  RegDesc { reg: Register::R12,     dwarf: Some(12), name: "r12" },
  RegDesc { reg: Register::R13,     dwarf: Some(13), name: "r13" },
  RegDesc { reg: Register::R14,     dwarf: Some(14), name: "r14" },
  RegDesc { reg: Register::R15,     dwarf: Some(15), name: "r15" },

  // Instruction pointer and flags
  RegDesc { reg: Register::Rip,     dwarf: None,     name: "rip" },
  RegDesc { reg: Register::RFlags,  dwarf: Some(49), name: "eflags" },

  // Segment registers
  RegDesc { reg: Register::Es,      dwarf: Some(50), name: "es" },
  RegDesc { reg: Register::Cs,      dwarf: Some(51), name: "cs" },
  RegDesc { reg: Register::Ss,      dwarf: Some(52), name: "ss" },
  RegDesc { reg: Register::Ds,      dwarf: Some(53), name: "ds" },
  RegDesc { reg: Register::Fs,      dwarf: Some(54), name: "fs" },
  RegDesc { reg: Register::Gs,      dwarf: Some(55), name: "gs" },

  // Segment bases
  RegDesc { reg: Register::FsBase,  dwarf: Some(58), name: "fs_base" },
  RegDesc { reg: Register::GsBase,  dwarf: Some(59), name: "gs_base" },

  // Syscall ABI
  RegDesc { reg: Register::OrigRax, dwarf: None,     name: "orig_rax" },
];

/* Values from: https://docs.rs/gimli/0.13.0/gimli/struct.UnwindTableRow.html#method.register */

pub fn get_register_value(pid: Pid, r: Register) -> Result<u64> { 
  let regs = ptrace::getregs(pid)?;
  let val = match r {
    Register::Rax => regs.rax,
    Register::Rdx => regs.rdx,
    Register::Rcx => regs.rcx,
    Register::Rbx => regs.rbx,

    Register::Rsi => regs.rsi,
    Register::Rdi => regs.rdi,
    Register::Rbp => regs.rbp,
    Register::Rsp => regs.rsp,

    Register::R8  => regs.r8,
    Register::R9  => regs.r9,
    Register::R10 => regs.r10,
    Register::R11 => regs.r11,
    Register::R12 => regs.r12,
    Register::R13 => regs.r13,
    Register::R14 => regs.r14,
    Register::R15 => regs.r15,

    Register::Rip    => regs.rip,
    Register::RFlags => regs.eflags,

    Register::Es => regs.es,
    Register::Cs => regs.cs,
    Register::Ss => regs.ss,
    Register::Ds => regs.ds,
    Register::Fs => regs.fs,
    Register::Gs => regs.gs,

    Register::FsBase => regs.fs_base,
    Register::GsBase => regs.gs_base,

    Register::OrigRax => regs.orig_rax,
  }; 
  Ok(val)
}

pub fn set_register_value(pid: Pid, r: Register, value: u64) -> Result<()> { 
  let mut regs = ptrace::getregs(pid)?; 
  match r {
    Register::Rax => regs.rax = value,
    Register::Rdx => regs.rdx = value,
    Register::Rcx => regs.rcx = value,
    Register::Rbx => regs.rbx = value,

    Register::Rsi => regs.rsi = value,
    Register::Rdi => regs.rdi = value,
    Register::Rbp => regs.rbp = value,
    Register::Rsp => regs.rsp = value,

    Register::R8  => regs.r8 = value,
    Register::R9  => regs.r9 = value,
    Register::R10 => regs.r10 = value,
    Register::R11 => regs.r11 = value,
    Register::R12 => regs.r12 = value,
    Register::R13 => regs.r13 = value,
    Register::R14 => regs.r14 = value,
    Register::R15 => regs.r15 = value,

    Register::Rip    => regs.rip = value,
    Register::RFlags => regs.eflags = value,

    Register::Es => regs.es = value,
    Register::Cs => regs.cs = value,
    Register::Ss => regs.ss = value,
    Register::Ds => regs.ds = value,
    Register::Fs => regs.fs = value,
    Register::Gs => regs.gs = value,

    Register::FsBase => regs.fs_base = value,
    Register::GsBase => regs.gs_base = value,

    Register::OrigRax => regs.orig_rax = value,
  }
  
  ptrace::setregs(pid, regs)
}

pub fn get_reg_val_from_dwarf(pid: Pid, dwarf_reg: u16) -> Result<Option<u64>> {
  let regs = ptrace::getregs(pid)?;

  let val = match dwarf_reg {
    0  => regs.rax,
    1  => regs.rdx,
    2  => regs.rcx,
    3  => regs.rbx,
    4  => regs.rsi,
    5  => regs.rdi,
    6  => regs.rbp,
    7  => regs.rsp,

    8  => regs.r8,
    9  => regs.r9,
    10 => regs.r10,
    11 => regs.r11,
    12 => regs.r12,
    13 => regs.r13,
    14 => regs.r14,
    15 => regs.r15,

    49 => regs.eflags,
    50 => regs.es,
    51 => regs.cs,
    52 => regs.ss,
    53 => regs.ds,
    54 => regs.fs,
    55 => regs.gs,

    58 => regs.fs_base,
    59 => regs.gs_base,

    _ => return Ok(None),
  };

  Ok(Some(val))
}

pub fn get_register_name(r: Register) -> &'static str {
  match r {
    Register::Rax => "rax",
    Register::Rdx => "rdx",
    Register::Rcx => "rcx",
    Register::Rbx => "rbx",

    Register::Rsi => "rsi",
    Register::Rdi => "rdi",
    Register::Rbp => "rbp",
    Register::Rsp => "rsp",

    Register::R8  => "r8",
    Register::R9  => "r9",
    Register::R10 => "r10",
    Register::R11 => "r11",
    Register::R12 => "r12",
    Register::R13 => "r13",
    Register::R14 => "r14",
    Register::R15 => "r15",

    Register::Rip    => "rip",
    Register::RFlags => "eflags",

    Register::Es => "es",
    Register::Cs => "cs",
    Register::Ss => "ss",
    Register::Ds => "ds",
    Register::Fs => "fs",
    Register::Gs => "gs",

    Register::FsBase => "fs_base",
    Register::GsBase => "gs_base",

    Register::OrigRax => "orig_rax",
  }
}

pub fn get_register_from_name(name: &str) -> Option<Register> {
  match name {
    "rax" | "RAX" => Some(Register::Rax),
    "rdx" | "RDX" => Some(Register::Rdx),
    "rcx" | "RCX" => Some(Register::Rcx),
    "rbx" | "RBX" => Some(Register::Rbx),

    "rsi" | "RSI" => Some(Register::Rsi),
    "rdi" | "RDI" => Some(Register::Rdi),
    "rbp" | "RBP" => Some(Register::Rbp),
    "rsp" | "RSP" => Some(Register::Rsp),

    "r8"  | "R8"  => Some(Register::R8),
    "r9"  | "R9"  => Some(Register::R9),
    "r10" | "R10" => Some(Register::R10),
    "r11" | "R11" => Some(Register::R11),
    "r12" | "R12" => Some(Register::R12),
    "r13" | "R13" => Some(Register::R13),
    "r14" | "R14" => Some(Register::R14),
    "r15" | "R15" => Some(Register::R15),

    "rip" | "RIP" => Some(Register::Rip),
    "eflags" | "rflags" | "EFLAGS" | "RFLAGS" => Some(Register::RFlags),

    "es" | "ES" => Some(Register::Es),
    "cs" | "CS" => Some(Register::Cs),
    "ss" | "SS" => Some(Register::Ss),
    "ds" | "DS" => Some(Register::Ds),
    "fs" | "FS" => Some(Register::Fs),
    "gs" | "GS" => Some(Register::Gs),

    "fs_base" | "FS_BASE" => Some(Register::FsBase),
    "gs_base" | "GS_BASE" => Some(Register::GsBase),

    "orig_rax" | "ORIG_RAX" => Some(Register::OrigRax),

    _ => None,
  }
}
