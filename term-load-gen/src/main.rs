//! Terminal Load Generator
//!
//! Generates controlled screen updates at adjustable rates and sizes
//! for measuring terminal rendering performance.
//!
//! Usage:
//!   term-load-gen [OPTIONS]
//!
//! Measure externally with `time`, `perf`, or other profiling tools.
//! For wezterm, use the debug overlay (Ctrl+Shift+L) to view metrics.
//!
//! Examples:
//!   # Single cell update at 120 Hz (best case - high cache hit rate)
//!   term-load-gen -r 120
//!
//!   # Full line updates at 60 Hz, sweeping down the screen
//!   term-load-gen -r 60 -w full -p sweep
//!
//!   # Scrolling output simulation
//!   term-load-gen -r 30 --scroll
//!
//!   # Random positions with varying colors (stress test)
//!   term-load-gen -r 60 -w 10 -p random --vary colors

use std::io::{self, Write};
use std::time::{Duration, Instant};

use clap::{Parser, ValueEnum};

#[derive(Debug, Clone, ValueEnum, PartialEq)]
enum Position {
    /// Stay at fixed position
    Fixed,
    /// Move sequentially through the screen
    Sweep,
    /// Random positions each frame
    Random,
}

#[derive(Debug, Clone, ValueEnum, PartialEq)]
enum Vary {
    /// No variation - same character and color
    None,
    /// Different characters each frame
    Chars,
    /// Different colors each frame
    Colors,
    /// Both characters and colors vary
    Both,
}

#[derive(Parser, Debug)]
#[command(name = "term-load-gen")]
#[command(about = "Terminal load generator for measuring rendering performance\n\nMeasure with external tools (time, perf, etc.) or use wezterm's debug overlay (Ctrl+Shift+L).")]
struct Args {
    // === Size ===
    /// Cells (width) per update. Use "full" for full terminal width. Default: 1 (full for scroll mode).
    #[arg(short = 'w', long, value_name = "N|full")]
    width: Option<String>,

    /// Lines (height) per update. Use "full" for full terminal height.
    #[arg(short = 'H', long, default_value = "1", value_name = "N|full")]
    height: String,

    // === Position ===
    /// Position mode: fixed, sweep, or random
    #[arg(short, long, default_value = "fixed", value_enum)]
    position: Position,

    /// Starting row for fixed/sweep modes
    #[arg(long, default_value = "0")]
    start_row: usize,

    /// Starting column for fixed/sweep modes
    #[arg(long, default_value = "0")]
    start_col: usize,

    // === Content ===
    /// Content variation: none, chars, colors, or both
    #[arg(long, default_value = "none", value_enum)]
    vary: Vary,

    // === Timing ===
    /// Update rate in Hz (updates per second)
    #[arg(short, long, default_value = "60")]
    rate: f64,

    /// Test duration in seconds (0 = infinite)
    #[arg(short, long, default_value = "0")]
    duration: u64,

    // === Special modes ===
    /// Scroll mode: append at bottom with newlines (simulates shell output)
    #[arg(long)]
    scroll: bool,

    /// Idle mode: no updates (baseline measurement)
    #[arg(long)]
    idle: bool,

    /// Prefill screen with random content before starting (simulates editing existing content)
    #[arg(long)]
    prefill: bool,

    // === Display ===
    /// Terminal width override (auto-detected if not specified)
    #[arg(long, value_name = "COLS")]
    term_width: Option<usize>,

    /// Terminal height override (auto-detected if not specified)
    #[arg(long, value_name = "ROWS")]
    term_height: Option<usize>,

    /// Quiet mode - minimal output
    #[arg(short, long)]
    quiet: bool,
}

fn get_terminal_size() -> (usize, usize) {
    // Try ioctl first
    if let Some((terminal_size::Width(w), terminal_size::Height(h))) = terminal_size::terminal_size() {
        return (w as usize, h as usize);
    }
    // Fall back to env vars
    let cols = std::env::var("COLUMNS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(80);
    let rows = std::env::var("LINES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(24);
    (cols, rows)
}

fn parse_dimension(s: &str, max: usize) -> usize {
    if s.eq_ignore_ascii_case("full") {
        max
    } else if s.ends_with('%') {
        let pct: f64 = s.trim_end_matches('%').parse().unwrap_or(100.0);
        ((pct / 100.0) * max as f64).max(1.0) as usize
    } else {
        s.parse().unwrap_or(1).min(max)
    }
}

// ANSI escape sequences
fn cursor_to(row: usize, col: usize) -> String {
    format!("\x1b[{};{}H", row + 1, col + 1)
}

fn set_color(fg: u8, bg: u8) -> String {
    format!("\x1b[38;5;{}m\x1b[48;5;{}m", fg, bg)
}

fn reset_color() -> &'static str {
    "\x1b[0m"
}

fn hide_cursor() -> &'static str {
    "\x1b[?25l"
}

fn show_cursor() -> &'static str {
    "\x1b[?25h"
}

fn clear_screen() -> &'static str {
    "\x1b[2J"
}

fn main() -> io::Result<()> {
    let args = Args::parse();

    let (term_cols, term_rows) = get_terminal_size();
    let term_width = args.term_width.unwrap_or(term_cols);
    let term_height = args.term_height.unwrap_or(term_rows);

    // Header takes up space in non-quiet mode
    let header_lines = if args.quiet { 0 } else { 4 };
    let status_lines = if args.quiet { 0 } else { 1 };
    let work_height = term_height.saturating_sub(header_lines + status_lines);
    let work_width = term_width;

    // Parse update dimensions
    // Scroll mode defaults to full width if not explicitly set
    let update_width = match &args.width {
        Some(w) => parse_dimension(w, work_width),
        None => if args.scroll { work_width } else { 1 },
    };
    let update_height = parse_dimension(&args.height, work_height);

    let interval = Duration::from_secs_f64(1.0 / args.rate);
    let duration = if args.duration == 0 {
        Duration::MAX
    } else {
        Duration::from_secs(args.duration)
    };

    let mut stdout = io::stdout().lock();

    // Setup
    write!(stdout, "{}{}", hide_cursor(), clear_screen())?;

    // Draw header
    if !args.quiet {
        write!(
            stdout,
            "{}Terminal Load Generator - Ctrl+C to stop",
            cursor_to(0, 0)
        )?;

        let mode_str = if args.idle {
            "idle".to_string()
        } else if args.scroll {
            "scroll".to_string()
        } else {
            format!("{}x{} {:?}", update_width, update_height, args.position)
        };

        write!(
            stdout,
            "{}Rate: {} Hz | Size: {} | Vary: {:?}",
            cursor_to(1, 0),
            args.rate,
            mode_str,
            args.vary
        )?;
        write!(
            stdout,
            "{}Ctrl+C to stop | Duration: {}s",
            cursor_to(2, 0),
            if args.duration == 0 {
                "infinite".to_string()
            } else {
                args.duration.to_string()
            }
        )?;
    }
    stdout.flush()?;

    let chars: Vec<char> = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789"
        .chars()
        .collect();
    let colors: Vec<u8> = (1..=231).collect();

    // Prefill screen with random content if requested
    if args.prefill && !args.scroll {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        for row in 0..work_height {
            write!(stdout, "{}", cursor_to(header_lines + row, 0))?;
            for col in 0..work_width {
                // Generate pseudo-random char based on position
                let mut hasher = DefaultHasher::new();
                (row * work_width + col).hash(&mut hasher);
                let hash = hasher.finish();
                let ch = chars[hash as usize % chars.len()];
                let fg = colors[hash as usize % colors.len()];
                let bg = colors[(hash >> 8) as usize % colors.len()];
                write!(stdout, "{}{}", set_color(fg, bg), ch)?;
            }
            write!(stdout, "{}", reset_color())?;
        }
        stdout.flush()?;
    }

    let start = Instant::now();
    let mut frame: u64 = 0;
    let mut pos_row: usize = header_lines + args.start_row;
    let mut pos_col: usize = args.start_col;

    // Stats
    let mut total_cells_updated: u64 = 0;
    let mut last_stats = Instant::now();
    let mut frames_since_stats: u64 = 0;

    loop {
        let frame_start = Instant::now();

        if start.elapsed() >= duration {
            break;
        }

        // Choose character based on variation mode
        let ch = match args.vary {
            Vary::Chars | Vary::Both => chars[frame as usize % chars.len()],
            _ => '#',
        };

        // Choose colors based on variation mode
        let (fg, bg) = match args.vary {
            Vary::Colors | Vary::Both => {
                let idx = frame as usize;
                (
                    colors[idx % colors.len()],
                    colors[(idx + 128) % colors.len()],
                )
            }
            _ => (15, 0), // White on black
        };

        // Generate update based on mode
        if args.idle {
            // No updates - just wait for baseline measurement
        } else if args.scroll {
            // Scroll mode: print lines and let terminal scroll naturally
            if args.quiet {
                // Quiet mode: just print, no positioning - natural scrolling
                write!(stdout, "{}", set_color(fg, bg))?;
                for i in 0..update_width {
                    let line_ch = match args.vary {
                        Vary::Chars | Vary::Both => chars[(frame as usize + i) % chars.len()],
                        _ => ch,
                    };
                    write!(stdout, "{}", line_ch)?;
                }
                write!(stdout, "{}\n", reset_color())?;
            } else {
                // Non-quiet: use scroll region to protect header/status
                if frame == 0 {
                    write!(stdout, "\x1b[{};{}r", header_lines + 1, term_height - status_lines)?;
                    write!(stdout, "\x1b[{};1H", term_height - status_lines)?;
                }
                write!(stdout, "{}", set_color(fg, bg))?;
                for i in 0..update_width {
                    let line_ch = match args.vary {
                        Vary::Chars | Vary::Both => chars[(frame as usize + i) % chars.len()],
                        _ => ch,
                    };
                    write!(stdout, "{}", line_ch)?;
                }
                write!(stdout, "{}\n", reset_color())?;
            }
            total_cells_updated += update_width as u64;
        } else {
            // Normal update: draw rectangle at current position

            // Calculate position based on mode
            let (row, col) = match args.position {
                Position::Fixed => (pos_row, pos_col),
                Position::Sweep => {
                    let r = pos_row;
                    let c = pos_col;
                    // Advance position for next frame
                    pos_col += update_width;
                    if pos_col + update_width > work_width {
                        pos_col = 0;
                        pos_row += update_height;
                        if pos_row + update_height > header_lines + work_height {
                            pos_row = header_lines;
                        }
                    }
                    (r, c)
                }
                Position::Random => {
                    use std::collections::hash_map::DefaultHasher;
                    use std::hash::{Hash, Hasher};

                    let mut hasher = DefaultHasher::new();
                    frame.hash(&mut hasher);
                    let hash = hasher.finish();

                    let max_row = work_height.saturating_sub(update_height).max(1);
                    let max_col = work_width.saturating_sub(update_width).max(1);

                    let r = header_lines + (hash as usize % max_row);
                    let c = (hash >> 32) as usize % max_col;
                    (r, c)
                }
            };

            // Draw the update rectangle
            for dy in 0..update_height {
                let draw_row = row + dy;
                if draw_row >= header_lines + work_height {
                    break;
                }
                write!(stdout, "{}{}", cursor_to(draw_row, col), set_color(fg, bg))?;
                for dx in 0..update_width.min(work_width - col) {
                    // Vary character per-cell within the block
                    let cell_ch = match args.vary {
                        Vary::Chars | Vary::Both => {
                            chars[(frame as usize + dy * update_width + dx) % chars.len()]
                        }
                        _ => ch,
                    };
                    write!(stdout, "{}", cell_ch)?;
                }
                write!(stdout, "{}", reset_color())?;
            }
            total_cells_updated += (update_width * update_height) as u64;
        }

        // Update stats display
        frames_since_stats += 1;
        if !args.quiet && last_stats.elapsed() >= Duration::from_millis(500) {
            let elapsed = start.elapsed().as_secs_f64();
            let fps = frames_since_stats as f64 / last_stats.elapsed().as_secs_f64();
            let cells_per_sec = total_cells_updated as f64 / elapsed.max(0.001);

            write!(
                stdout,
                "{}{}Frame: {:8} | FPS: {:6.1} | Cells/s: {:10.0}{}",
                cursor_to(term_height - 1, 0),
                set_color(0, 7),
                frame,
                fps,
                cells_per_sec,
                reset_color()
            )?;

            frames_since_stats = 0;
            last_stats = Instant::now();
        }

        stdout.flush()?;
        frame += 1;

        // Sleep to maintain target rate
        let frame_time = frame_start.elapsed();
        if frame_time < interval {
            std::thread::sleep(interval - frame_time);
        }
    }

    // Cleanup and show final stats
    let elapsed = start.elapsed().as_secs_f64();
    // Reset scroll region if we set one
    if args.scroll && !args.quiet {
        write!(stdout, "\x1b[r")?; // Reset scroll region to full screen
    }
    write!(stdout, "{}{}", clear_screen(), show_cursor())?;

    let mode_str = if args.idle {
        "idle".to_string()
    } else if args.scroll {
        "scroll".to_string()
    } else {
        format!("{}x{} {:?}", update_width, update_height, args.position)
    };

    println!("Load Generator Results");
    println!("======================");
    println!();
    println!("Timing:");
    println!("  Duration:     {:>10.2}s", elapsed);
    println!("  Total frames: {:>10}", frame);
    println!("  Average FPS:  {:>10.2}", frame as f64 / elapsed);
    println!("  Target FPS:   {:>10.2}", args.rate);
    println!();
    println!("Throughput:");
    println!("  Total cells:  {:>10}", total_cells_updated);
    println!("  Cells/second: {:>10.0}", total_cells_updated as f64 / elapsed);
    println!();
    println!("Configuration:");
    println!("  Mode:         {}", mode_str);
    println!("  Vary:         {:?}", args.vary);
    println!("  Terminal:     {}x{}", term_width, term_height);
    println!();
    println!("Use external tools (time, perf) or wezterm debug overlay (Ctrl+Shift+L) for metrics.");

    stdout.flush()?;
    Ok(())
}
