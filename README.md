# Description
An x86 ptrace based linux debugger. Commands you can run: 
- continue (to continue execution of program) 
- break (to set a breakpoint, both address and source level supported) 
- next (Execute next source line, skip over function calls)
- step (step into function)
- stepi (single instruction step)
- finish (step out of current function)
- register
  - dump print all registers in hex
  - read print value of ONE register
  - write set register value in hex
- memory 
  - read Read a word at that dadress
  - write Write a word at that address

Example flow: 
Hitting a breakpoint
```
(rust_dbg) break main.c:5
Breakpoint set at main.c:5 (0x555555555149)
(rust_dbg) c
Stopped by SIGTRAP
PC = 0x555555555149
At main (main.c:5)
main.c:5
int main() { 
(rust_dbg) n
Stopped by SIGTRAP
PC = 0x555555555151
At main (main.c:6)
main.c:6
  printf("Hello world\n"); 
```

# TODO:
- stack trace
- variable handling

# Sources
Based on: https://tartanllama.xyz/posts/writing-a-linux-debugger/setup/ 

The reason I decided to write the debugger in rust rather than C++ was to avoid blindly copy pasting code snippets from the guide above (which was written for C++);


