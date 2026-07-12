# Build the empyrean C-channel validation runner.
#
# Links against libempyrean.dylib produced by `cargo build -p
# empyrean-c --release`. Run from this directory:
#   make
# The binary lands at ./runner.

EMPYREAN_ROOT := ../../../empyrean
INCLUDE_DIR := $(EMPYREAN_ROOT)/include
LIB_DIR := $(EMPYREAN_ROOT)/target/release

CC := cc
# -std=c11 keeps strict ISO C syntax, but under it glibc hides POSIX/BSD
# library functions (clock_gettime / CLOCK_MONOTONIC, etc.) unless a
# feature-test macro is set — macOS headers expose them regardless, so
# this only bites on Linux. _DEFAULT_SOURCE enables glibc's full default
# function set without loosening the language dialect.
CFLAGS := -O2 -Wall -Wextra -std=c11 -D_DEFAULT_SOURCE -I$(INCLUDE_DIR)
LDFLAGS := -L$(LIB_DIR) -lempyrean -Wl,-rpath,$(abspath $(LIB_DIR))

runner: runner.c $(INCLUDE_DIR)/empyrean.h
	$(CC) $(CFLAGS) -o $@ $< $(LDFLAGS)

clean:
	rm -f runner

.PHONY: clean
