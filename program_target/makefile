CC = gcc
CFLAGS_DEBUG = -g -O0
CFLAGS_OPTIMIZED = -g -O2

# Default target
all: main test_step test_loop

# Main test program
main: main.c
	$(CC) $(CFLAGS_DEBUG) main.c -o main

# Stepping test program
test_step: test_step.c
	$(CC) $(CFLAGS_DEBUG) test_step.c -o test_step

# Loop/conditional test program  
test_loop: test_loop.c
	$(CC) $(CFLAGS_DEBUG) test_loop.c -o test_loop

# Optimized versions (may produce non-contiguous function ranges)
test_step_opt: test_step.c
	$(CC) $(CFLAGS_OPTIMIZED) test_step.c -o test_step_opt

test_loop_opt: test_loop.c
	$(CC) $(CFLAGS_OPTIMIZED) test_loop.c -o test_loop_opt

# Segfault test
seg_fault: seg_fault.c
	$(CC) $(CFLAGS_DEBUG) seg_fault.c -o seg_fault

# Build all optimized versions
optimized: test_step_opt test_loop_opt

# Clean up
clean:
	rm -f main test_step test_loop test_step_opt test_loop_opt seg_fault

.PHONY: all optimized clean