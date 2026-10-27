//! This example shows how to query the terminal default foreground
//! and background colors via `OSC 10` and `OSC 11`.
//! It then transforms and blends those RGB colors to drive a
//! smooth animated text effect.

use std::f32::consts::PI;
use std::thread;
use std::time::{Duration, Instant};

use termwiz::caps::Capabilities;
use termwiz::cell::{AttributeChange, Intensity};
use termwiz::color::{ColorAttribute, SrgbaTuple};
use termwiz::escape::osc::DynamicColorNumber;
use termwiz::input::{InputEvent, KeyCode, KeyEvent, Modifiers};
use termwiz::surface::{Change, CursorVisibility, Position};
use termwiz::terminal::{new_terminal, Terminal};

const DEMO_TEXT: &str =
    "interpolation turns monochromatic approximation into phosphorescent shimmer";
const TICK: Duration = Duration::from_millis(16);
const WAVE_SWEEP_SECS: f32 = 2.0;
const MIN_BLEND_ALPHA: f32 = 0.32;
const WAVE_CYCLE_CHARS: f32 = 14.0;
const WAVE_CYCLES_PER_SWEEP: f32 = 3.0;

#[derive(Clone, Copy)]
struct DefaultColors {
    fg: (u8, u8, u8),
    bg: (u8, u8, u8),
}

fn main() -> termwiz::Result<()> {
    let caps = Capabilities::new_from_env()?;
    let mut terminal = new_terminal(caps)?;
    terminal.set_raw_mode()?;
    terminal.enter_alternate_screen()?;

    let probed_colors = probe_default_colors(&mut terminal);
    let result = run_demo(&mut terminal, probed_colors);

    terminal.render(&[Change::CursorVisibility(CursorVisibility::Visible)])?;
    terminal.flush()?;

    result
}

fn run_demo<T: Terminal>(
    terminal: &mut T,
    probed_colors: Option<DefaultColors>,
) -> termwiz::Result<()> {
    let start = Instant::now();

    loop {
        draw_frame(terminal, start.elapsed(), probed_colors)?;

        loop {
            match terminal.poll_input(Some(Duration::ZERO))? {
                Some(InputEvent::Key(KeyEvent {
                    key: KeyCode::Escape,
                    ..
                })) => return Ok(()),
                Some(InputEvent::Key(KeyEvent {
                    key: KeyCode::Char('q' | 'Q'),
                    ..
                })) => return Ok(()),
                Some(InputEvent::Key(KeyEvent {
                    key: KeyCode::Char('c' | 'C'),
                    modifiers,
                })) if modifiers.contains(Modifiers::CTRL) => return Ok(()),
                Some(_) => {}
                None => break,
            }
        }

        thread::sleep(TICK);
    }
}

fn draw_frame<T: Terminal>(
    terminal: &mut T,
    elapsed: Duration,
    probed_colors: Option<DefaultColors>,
) -> termwiz::Result<()> {
    let mut changes = vec![
        Change::CursorVisibility(CursorVisibility::Hidden),
        Change::ClearScreen(ColorAttribute::Default),
    ];

    let active_colors = probed_colors.unwrap_or(DefaultColors {
        fg: (255, 255, 255),
        bg: (0, 0, 0),
    });

    write_label(&mut changes, 2, 1, "Dynamic color wave demo");
    write_plain(
        &mut changes,
        2,
        3,
        "Press q, Esc, or Ctrl-C to exit.",
        Intensity::Bold,
    );
    write_plain(
        &mut changes,
        2,
        5,
        "Top: no color query, only coarse DIM / BOLD bands.",
        Intensity::Normal,
    );
    write_plain(
        &mut changes,
        2,
        9,
        "Bottom: query default bg/fg, then blend through a smooth ramp.",
        Intensity::Normal,
    );

    match probed_colors {
        Some(colors) => write_plain(
            &mut changes,
            2,
            11,
            &format!(
                "Probed defaults: fg=rgb({},{},{}) bg=rgb({},{},{})",
                colors.fg.0, colors.fg.1, colors.fg.2, colors.bg.0, colors.bg.1, colors.bg.2
            ),
            Intensity::Normal,
        ),
        None => write_plain(
            &mut changes,
            2,
            11,
            "Probe unavailable; blend line uses fallback fg=white bg=black.",
            Intensity::Normal,
        ),
    };

    write_wave_line(
        &mut changes,
        4,
        7,
        DEMO_TEXT,
        elapsed,
        WaveMode::Discrete,
        active_colors,
    );
    write_wave_line(
        &mut changes,
        4,
        13,
        DEMO_TEXT,
        elapsed,
        WaveMode::Blend,
        active_colors,
    );

    terminal.render(&changes)?;
    terminal.flush()?;
    Ok(())
}

fn probe_default_colors<T: Terminal>(terminal: &mut T) -> Option<DefaultColors> {
    let mut probe = terminal.probe_capabilities()?;
    let fg = probe
        .dynamic_color(DynamicColorNumber::TextForegroundColor)
        .ok()
        .map(srgba_to_rgb);
    let bg = probe
        .dynamic_color(DynamicColorNumber::TextBackgroundColor)
        .ok()
        .map(srgba_to_rgb);

    fg.zip(bg).map(|(fg, bg)| DefaultColors { fg, bg })
}

fn write_wave_line(
    changes: &mut Vec<Change>,
    x: usize,
    y: usize,
    text: &str,
    elapsed: Duration,
    mode: WaveMode,
    colors: DefaultColors,
) {
    let chars: Vec<char> = text.chars().collect();
    let sweep_phase = (elapsed.as_secs_f32() % WAVE_SWEEP_SECS) / WAVE_SWEEP_SECS;
    let phase = sweep_phase * WAVE_CYCLE_CHARS * WAVE_CYCLES_PER_SWEEP;

    for (idx, ch) in chars.into_iter().enumerate() {
        let intensity = wave_intensity(idx as f32, phase);

        changes.push(Change::CursorPosition {
            x: Position::Absolute(x + idx),
            y: Position::Absolute(y),
        });

        match mode {
            WaveMode::Discrete => {
                changes.push(Change::Attribute(AttributeChange::Foreground(
                    ColorAttribute::Default,
                )));
                changes.push(Change::Attribute(AttributeChange::Intensity(
                    discrete_intensity(intensity),
                )));
            }
            WaveMode::Blend => {
                let alpha = MIN_BLEND_ALPHA + (1.0 - MIN_BLEND_ALPHA) * intensity.clamp(0.0, 1.0);
                let blended = blend(colors.fg, colors.bg, alpha);
                changes.push(Change::Attribute(AttributeChange::Foreground(
                    ColorAttribute::TrueColorWithDefaultFallback(SrgbaTuple::from(blended)),
                )));
                changes.push(Change::Attribute(AttributeChange::Intensity(
                    Intensity::Normal,
                )));
            }
        }

        changes.push(Change::Text(ch.to_string().into()));
    }

    changes.push(Change::Attribute(AttributeChange::Foreground(
        ColorAttribute::Default,
    )));
    changes.push(Change::Attribute(AttributeChange::Intensity(
        Intensity::Normal,
    )));
}

fn wave_intensity(i_pos: f32, phase: f32) -> f32 {
    let angle = (i_pos - phase) * (2.0 * PI / WAVE_CYCLE_CHARS);
    0.5 * (1.0 + angle.cos())
}

fn write_label(changes: &mut Vec<Change>, x: usize, y: usize, text: &str) {
    write_plain(changes, x, y, text, Intensity::Bold);
}

fn write_plain(changes: &mut Vec<Change>, x: usize, y: usize, text: &str, intensity: Intensity) {
    changes.push(Change::CursorPosition {
        x: Position::Absolute(x),
        y: Position::Absolute(y),
    });
    changes.push(Change::Attribute(AttributeChange::Foreground(
        ColorAttribute::Default,
    )));
    changes.push(Change::Attribute(AttributeChange::Intensity(intensity)));
    changes.push(Change::Text(text.into()));
    changes.push(Change::Attribute(AttributeChange::Intensity(
        Intensity::Normal,
    )));
}

fn srgba_to_rgb(color: SrgbaTuple) -> (u8, u8, u8) {
    let (r, g, b, _) = color.to_srgb_u8();
    (r, g, b)
}

fn blend(fg: (u8, u8, u8), bg: (u8, u8, u8), alpha: f32) -> (u8, u8, u8) {
    let r = (fg.0 as f32 * alpha + bg.0 as f32 * (1.0 - alpha)) as u8;
    let g = (fg.1 as f32 * alpha + bg.1 as f32 * (1.0 - alpha)) as u8;
    let b = (fg.2 as f32 * alpha + bg.2 as f32 * (1.0 - alpha)) as u8;
    (r, g, b)
}

fn discrete_intensity(intensity: f32) -> Intensity {
    if intensity < 0.7 {
        Intensity::Half
    } else {
        Intensity::Bold
    }
}

#[derive(Clone, Copy)]
enum WaveMode {
    Discrete,
    Blend,
}
