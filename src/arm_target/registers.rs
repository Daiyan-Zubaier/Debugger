#[derive(Clone, Copy)]
pub(super) struct ArmRegisterDesc {
  pub(super) name: &'static str,
  pub(super) index: usize,
}

pub(super) const ARM_CORE_REGS: [ArmRegisterDesc; 17] = [
  ArmRegisterDesc {
    name: "r0",
    index: 0,
  },
  ArmRegisterDesc {
    name: "r1",
    index: 1,
  },
  ArmRegisterDesc {
    name: "r2",
    index: 2,
  },
  ArmRegisterDesc {
    name: "r3",
    index: 3,
  },
  ArmRegisterDesc {
    name: "r4",
    index: 4,
  },
  ArmRegisterDesc {
    name: "r5",
    index: 5,
  },
  ArmRegisterDesc {
    name: "r6",
    index: 6,
  },
  ArmRegisterDesc {
    name: "r7",
    index: 7,
  },
  ArmRegisterDesc {
    name: "r8",
    index: 8,
  },
  ArmRegisterDesc {
    name: "r9",
    index: 9,
  },
  ArmRegisterDesc {
    name: "r10",
    index: 10,
  },
  ArmRegisterDesc {
    name: "r11",
    index: 11,
  },
  ArmRegisterDesc {
    name: "r12",
    index: 12,
  },
  ArmRegisterDesc {
    name: "sp",
    index: 13,
  },
  ArmRegisterDesc {
    name: "lr",
    index: 14,
  },
  ArmRegisterDesc {
    name: "pc",
    index: 15,
  },
  ArmRegisterDesc {
    name: "xpsr",
    index: 16,
  },
];

/// Look up an ARM register descriptor by user-facing name
pub(super) fn arm_register_from_name(name: &str) -> Option<ArmRegisterDesc> {
  let lower = name.to_ascii_lowercase();
  match lower.as_str() {
    "r13" => ARM_CORE_REGS.get(13).copied(),
    "r14" => ARM_CORE_REGS.get(14).copied(),
    "r15" => ARM_CORE_REGS.get(15).copied(),
    _ => ARM_CORE_REGS
      .iter()
      .copied()
      .find(|reg| reg.name == lower.as_str()),
  }
}
