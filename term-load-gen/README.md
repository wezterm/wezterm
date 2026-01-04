# term-load-gen

A terminal load generator for measuring rendering performance.

## Overview

This tool generates controlled terminal updates at configurable rates and patterns.
Use external measurement tools (`time`, `perf`, etc.) to analyze performance, or
wezterm's built-in debug overlay for renderer-specific metrics.

## Building

```bash
cd term-load-gen
cargo build --release
```

## Usage

```bash
# Run with a fixed duration and measure CPU usage
time wezterm start -- ./target/release/term-load-gen -d 10 -q

# Or run interactively and use wezterm's debug overlay (Ctrl+Shift+L)
wezterm start -- ./target/release/term-load-gen
```

## Options

All options are orthogonal and can be combined freely.

### Size

| Option | Description |
|--------|-------------|
| `-w, --width <N\|full>` | Cells per update (default: 1) |
| `-H, --height <N\|full>` | Lines per update (default: 1) |

Values can be a number, `full` for terminal size, or a percentage like `50%`.

### Position

| Option | Description |
|--------|-------------|
| `-p, --position <MODE>` | `fixed`, `sweep`, or `random` (default: fixed) |
| `--start-row <N>` | Starting row for fixed/sweep modes |
| `--start-col <N>` | Starting column for fixed/sweep modes |

### Content

| Option | Description |
|--------|-------------|
| `--vary <MODE>` | `none`, `chars`, `colors`, or `both` (default: none) |

### Timing

| Option | Description |
|--------|-------------|
| `-r, --rate <HZ>` | Updates per second (default: 60) |
| `-d, --duration <SEC>` | Test duration, 0 = infinite (default: 0) |

### Special Modes

| Option | Description |
|--------|-------------|
| `--scroll` | Append-at-bottom mode (simulates shell output) |
| `--idle` | No updates (baseline measurement) |
| `--prefill` | Fill screen with random content before starting (simulates editing) |

### Display

| Option | Description |
|--------|-------------|
| `--term-width <COLS>` | Override terminal width |
| `--term-height <ROWS>` | Override terminal height |
| `-q, --quiet` | Minimal output |

## Measuring Performance

### CPU Usage with `time`

```bash
# Measure total CPU time for a 10-second run
time wezterm start -- term-load-gen -d 10 -q

# Output:
# real    0m10.05s   # Wall clock time
# user    0m0.85s    # CPU time in user space (rendering)
# sys     0m0.12s    # CPU time in kernel (display/compositor)
```

Use `-q` (quiet) to minimize I/O overhead in measurements.

### Comparing Backends

```bash
# Test with different wezterm front_end settings
for backend in OpenGL WebGpu Software; do
  echo "=== $backend ==="
  time wezterm start --config "front_end='$backend'" -- term-load-gen -d 10 -q
done
```

### Profiling with `perf`

```bash
# Record performance data
perf record -g wezterm start -- term-load-gen -d 10 -q

# Analyze
perf report
```

## Examples

### Cache Performance Tests

```bash
# Single cell, fixed position - best case for line cache
term-load-gen -r 120

# Full line updates, sweeping down - tests line cache invalidation
term-load-gen -w full -p sweep -r 60

# Full screen updates - worst case, no cache benefit
term-load-gen -w full -H full -r 30
```

### Glyph Cache Tests

```bash
# Varying characters - tests glyph cache hit rate
term-load-gen -w 20 --vary chars -r 60

# Varying colors - tests color handling
term-load-gen -w 20 --vary colors -r 60

# Both varying - maximum glyph cache pressure
term-load-gen -w full --vary both -r 60
```

### Dirty Region Tracking Tests

```bash
# Random positions - tests dirty region coalescing
term-load-gen -w 10 -H 5 -p random -r 60

# Sweeping block - tests sequential dirty regions
term-load-gen -w 20 -H 3 -p sweep -r 60
```

### Real-World Simulations

```bash
# Scrolling shell output
term-load-gen --scroll -r 30

# Fast scrolling (e.g., compilation output)
term-load-gen --scroll -r 120 --vary chars

# Idle terminal - baseline CPU usage
term-load-gen --idle -d 30
```

## Automated Benchmark Script

The `benchmark-render.pl` script automates comparison between rendering backends:

```bash
# Quick benchmark (5 scenarios, 1 run each)
./benchmark-render.pl --quick --runs 1 --duration 8

# Full benchmark (8 scenarios, 3 runs each)
./benchmark-render.pl --duration 10
```

The script:
- Compares Software (llvmpipe) vs Cairo2D backends
- Captures CPU time, memory usage, and wezterm internal stats
- Runs multiple scenarios: idle, typing, single-cell, full-screen, scrolling
- Generates a summary report with improvement percentages

## Tips

1. **Use quiet mode for measurements**: `-q` reduces output overhead
2. **Fixed duration**: Always use `-d` for reproducible measurements
3. **Warm up**: The first run may be slower due to JIT/caching
4. **Multiple runs**: Average several runs for reliable results
5. **Isolate variables**: Change one option at a time
6. **Compare backends**: Test the same load across different renderers
