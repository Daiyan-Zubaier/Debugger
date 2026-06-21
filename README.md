A small Rust debugger for Cortex-M Micros and x86 Linux, has TUI 🤯 No one is using an IDE 🗣️

## Pics
![alt text](image.png)
Debugging main.c in program_target/

![alt text](image-1.png)
Debugging some simple non HAL code I wrote for the STM32 a while back.
## Linux x86-64 Mode

```sh
cargo run -- ./program_target/main
```

TUI mode:

```sh
cargo run -- --tui ./program_target/main
```

Commands:

- `continue` or `c`: continue execution
- `break 0x<addr>`: set an address breakpoint
- `break <filename>:<line>`: set a source breakpoint
- `next` or `n`: execute the next source line, stepping over calls
- `step` or `s`: step into calls until the source line changes
- `stepi`: single instruction step
- `finish`: step out of the current function
- `backtrace` or `bt`: print a frame-pointer based call stack
- `register dump`: print all registers
- `register read <reg>`: read one register
- `register write <reg> 0x<value>`: write one register
- `memory read 0x<addr>`: read a word
- `memory write 0x<addr> 0x<value>`: write a word

Backtrace currently walks the `RBP` frame chain. Compile the debuggee with frame pointers for best results.

## ARM Cortex-M Mode
^ This lets me debug on my stm32F401 Nucleo Board

Start OpenOCD in another terminal. Then connect this debugger to OpenOCD's GDB server:

```sh
cargo run -- --arm-gdb <pathp to ELF>
```

TUI mode:

```sh
cargo run -- --tui --arm-gdb <pathp to ELF>
```

The default endpoint is `127.0.0.1:3333`. You can override it:

```sh
cargo run -- --arm-gdb <pathp to ELF> 127.0.0.1:3333
```

ARM commands:

- `continue` or `c`: continue execution
- `stepi` or `si`: single instruction step
- `step`, `s`: source-level single step by repeated instruction stepping
- `next`, `n`: step until the source line changes, stepping over calls
- `break 0x<addr>`: set a breakpoint
- `break <filename>:<line>`: set a source breakpoint
- `clear 0x<addr>`: clear a tracked breakpoint
- `register dump`: print core registers
- `register read <reg>`: read a core register
- `register write <reg> 0x<value>`: write a core register
- `memory read 0x<addr> [len]`: read memory
- `memory write 0x<addr> 0x<value>`: write one 32-bit word
- `backtrace` or `bt`: print current frame plus likely callers found on the stack
- `halt`: interrupt the target
- `reset`: run `monitor reset halt`
- `monitor <cmd>`: send an OpenOCD monitor command
- `quit` or `q`: leave the debugger

The ARM backend uses GDB remote hardware breakpoints (`Z1`) first, then falls back to software breakpoints (`Z0`). For STM32 flash code, hardware breakpoints are normally what you want.

The ARM backtrace is intentionally conservative. It prints the current PC and scans the stack for Thumb return addresses that resolve to functions in the ELF. This is useful but not a real DWARF/EXIDX unwinder yet.

## TUI Shortcuts
Shortcuts:
- Ctrl + x - Switch panes
- Ctrl + c - halt execution for the target program
- Ctrl + q - Quit the debugger
- Ctrl + p - Scroll up in command history
- Ctrl + q - Scroll down in command history
- F5       - Same as running `continue` in the command window
- F10      - Same as running `step` in the command window
- F11      - Same as running `stepi` in the command window
- PageUp   - Scroll up in the highlighted pane
- PageDown - Scroll down in the highligted pane
- Escape   - Command window specific: Lets you clear your currently typed command

## Limitations
Does GDB have competition? Sorta...?

- Linux mode is x86-64 only.
- Linux breakpoints are hardcoded to x86 `0xcc`.
- Linux `finish` and `backtrace` assume useful frame pointers.
- ARM mode assumes an external GDB remote server such as OpenOCD.
- ARM mode does not flash firmware by itself. Use your normal embedded build/flash flow or OpenOCD monitor commands.
- ARM source stepping is implemented as repeated instruction stepping, not compiler-aware statement stepping.
- ARM backtrace is stack-scan based, not a real unwinder.
- The TUI is command-driven and currently does not have scrolling or async target execution.

## Sources

The original Linux debugger follows the shape of this [guide](https://tartanllama.xyz/posts/writing-a-linux-debugger/setup/) rewritten in Rust.

For the gdb packets see [here](https://sourceware.org/gdb/current/onlinedocs/gdb.html/Packets.html#insert-breakpoint-or-watchpoint-packet)
