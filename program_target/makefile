CC = gcc
CFLAGS_DEBUG = -g -O0
CFLAGS_OPTIMIZED = -g -O2

BUILD_DIR = build

# Default target
all: main test_step test_loop seg_fault

# Ensure build directory exists
$(BUILD_DIR):
	mkdir -p $(BUILD_DIR)

# Main test program
main: main.c | $(BUILD_DIR)
	$(CC) $(CFLAGS_DEBUG) $< -o $(BUILD_DIR)/$@

# Stepping test program
test_step: test_step.c | $(BUILD_DIR)
	$(CC) $(CFLAGS_DEBUG) $< -o $(BUILD_DIR)/$@

# Loop/conditional test program
test_loop: test_loop.c | $(BUILD_DIR)
	$(CC) $(CFLAGS_DEBUG) $< -o $(BUILD_DIR)/$@

# Optimized versions (may produce non-contiguous function ranges)
test_step_opt: test_step.c | $(BUILD_DIR)
	$(CC) $(CFLAGS_OPTIMIZED) $< -o $(BUILD_DIR)/$@

test_loop_opt: test_loop.c | $(BUILD_DIR)
	$(CC) $(CFLAGS_OPTIMIZED) $< -o $(BUILD_DIR)/$@

# Segfault test
seg_fault: seg_fault.c | $(BUILD_DIR)
	$(CC) $(CFLAGS_DEBUG) $< -o $(BUILD_DIR)/$@

# Build all optimized versions
optimized: test_step_opt test_loop_opt

clean:
	rm -rf $(BUILD_DIR)

.PHONY: all optimized clean
