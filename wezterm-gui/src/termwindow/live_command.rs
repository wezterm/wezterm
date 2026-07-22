//! Runs an arbitrary command attached to a hidden pty and renders its live
//! terminal output into an RGBA buffer, for use as a `BackgroundSource::Command`
//! layer. Unlike an animated gif/png (a bounded, pre-decoded set of frames),
//! this re-renders directly from the process's live, unbounded output on a
//! timer.
//!
//! The pty + headless terminal model half of this mirrors how a real pane is
//! spawned (see `mux::domain::LocalDomain::spawn_pane`), just without any of
//! the mux/pane machinery: `wezterm_term::Terminal` is explicitly documented
//! as usable without a gui or a real pty writer, so we feed it bytes directly
//! from our own reader thread.
//!
//! The rasterization half reuses `wezterm-font`'s shaper/rasterizer plus an
//! in-memory (CPU-only, no GPU context needed) `GlyphCache`, the same pieces
//! used by `wezterm ls-fonts --rasterize-ascii`, and blits glyph bitmaps onto
//! a plain buffer using the same bearing/offset math as the real per-frame
//! renderer (`termwindow::render::screen_line`).

use crate::glyphcache::GlyphCache;
use crate::utilsprites::RenderMetrics;
use anyhow::Context;
use config::CommandSource;
use config::TermConfig;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::cell::RefCell;
use std::io::Read;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use termwiz::image::{ImageData, ImageDataType};
use wezterm_font::shaper::PresentationWidth;
use wezterm_font::FontConfiguration;
use wezterm_term::{Terminal, TerminalSize};
use window::bitmaps::atlas::Sprite;
use window::bitmaps::ImageTexture;
use window::BitmapImage;

/// Cache key for a `LiveCommand` instance: its command line plus the
/// terminal grid size (in cells) it was spawned at. Keying on size (rather
/// than resizing an existing instance in place) keeps spawn/teardown as
/// the only lifecycle to worry about; see the comment on
/// `TermWindow::resolve_live_command` for how a size change is handled.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LiveCommandKey {
    pub argv: Vec<String>,
    pub cwd: Option<String>,
    pub width: usize,
    pub height: usize,
    /// `f32` doesn't implement `Eq`/`Hash`; bit-for-bit comparison is fine
    /// here since this always comes from the same unmodified config value,
    /// never a computed/varying float.
    pub font_scale_bits: u32,
}

/// `LiveCommand` holds a `GlyphCache`, which is `Rc`-based internally and
/// therefore neither `Send` nor `Sync`. That's fine — it's only ever
/// accessed from the render thread that owns the `TermWindow` caching it —
/// but it does mean `RefCell` (single-threaded interior mutability) is the
/// right tool here, not `Mutex`, matching how the *real* (GPU-backed)
/// glyph cache used for interactive panes is also stored behind a
/// `RefCell` on `RenderState` rather than a `Mutex`.
pub struct LiveCommand {
    inner: RefCell<Inner>,
}

struct Inner {
    // Keeping the child and master pty handles alive for as long as this
    // struct lives is what keeps the command running; dropping either one
    // is how we tear it down (see `Drop` below).
    child: Box<dyn Child + Send + Sync>,
    _master: Box<dyn MasterPty + Send>,
    terminal: Arc<Mutex<Terminal>>,
    glyph_cache: GlyphCache,
    fonts: Rc<FontConfiguration>,
    render_metrics: RenderMetrics,
    cols: usize,
    rows: usize,
    last_rendered: Instant,
    last_image: Arc<ImageData>,

    // Frame-to-frame dirty-row cache: re-shaping and re-rasterizing every
    // cell of every row on every tick (regardless of whether it actually
    // changed) is what made this noticeably slower than a real terminal
    // emulator driving the same effect (eg: the xterm the older
    // cmatrix-bg.sh script used) once there are enough cells on screen.
    // A real terminal only repaints what changed; this reuses that same
    // idea by hashing each row's content and skipping the expensive part
    // for rows whose hash matches the previous frame's, editing
    // `prev_buf` in place instead of building a fresh buffer from scratch
    // every time.
    prev_buf: Vec<u8>,
    prev_dims: (usize, usize),
    // Per row, the hash of each cluster in that row (in order) as of the
    // last frame. Diffing at cluster granularity (not whole-row) matters
    // for something like cmatrix specifically: it's dense enough that
    // *some* column in almost every row changes on almost every tick, so
    // a whole-row check would rarely skip anything. Clusters end up close
    // to per-character anyway, since cmatrix gives many characters their
    // own random color (a new color attribute starts a new cluster), so
    // this ends up skipping work at roughly the same granularity as the
    // effect's actual sparsity.
    prev_cluster_hashes: Vec<Vec<u64>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        // The background command has no other way to know we're done with
        // it; if we don't do this explicitly it leaks as an orphaned
        // process (eg: cmatrix would keep spinning in the background
        // forever after the layer that used it is removed or the window
        // closes).
        let _ = self.child.kill();
    }
}

impl LiveCommand {
    pub fn spawn(
        cmd: &CommandSource,
        width: usize,
        height: usize,
        base_render_metrics: &RenderMetrics,
        base_fonts: &Rc<FontConfiguration>,
    ) -> anyhow::Result<Self> {
        // A smaller `font_scale` needs glyphs actually rasterized at a
        // smaller point size (not just laid out on a scaled-down grid),
        // so this builds its own independent FontConfiguration/metrics
        // rather than reusing the window's -- otherwise the glyph bitmaps
        // would still be full-size and just overlap on the denser grid.
        let (fonts, render_metrics): (Rc<FontConfiguration>, RenderMetrics) =
            if (cmd.font_scale - 1.0).abs() < 0.001 {
                (Rc::clone(base_fonts), base_render_metrics.clone())
            } else {
                let scaled_config = config::configuration()
                    .with_font_size(config::configuration().font_size * cmd.font_scale as f64);
                let dpi = scaled_config
                    .dpi
                    .unwrap_or_else(|| ::window::default_dpi()) as usize;
                let fonts = Rc::new(FontConfiguration::new(Some(scaled_config), dpi)?);
                let render_metrics = RenderMetrics::new(&fonts)?;
                (fonts, render_metrics)
            };

        let cell_width = render_metrics.cell_size.width.max(1) as usize;
        let cell_height = render_metrics.cell_size.height.max(1) as usize;
        let cols = (width / cell_width).max(1);
        let rows = (height / cell_height).max(1);

        anyhow::ensure!(
            !cmd.argv.is_empty(),
            "background Command source requires a non-empty argv"
        );
        let argv: Vec<std::ffi::OsString> =
            cmd.argv.iter().map(|s| std::ffi::OsString::from(s.as_str())).collect();
        let mut builder = CommandBuilder::from_argv(argv);
        // `CommandBuilder` otherwise just inherits wezterm-gui's own
        // environment as-is. That's fine for TERM when launched from a
        // shell, but wezterm-gui is commonly launched from a desktop icon
        // (no controlling terminal, no TERM in its environment at all) —
        // in which case an ncurses program like cmatrix fails to
        // initialize and exits immediately. Real panes don't hit this
        // because pane spawning always goes through `apply_cmd_defaults`,
        // which explicitly sets TERM/COLORTERM/TERM_PROGRAM regardless of
        // the parent's environment; do the same here.
        let default_cwd = cmd.cwd.as_ref().map(std::path::PathBuf::from);
        config::configuration().apply_cmd_defaults(&mut builder, None, default_cwd.as_ref());

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: rows as u16,
                cols: cols as u16,
                pixel_width: (cell_width * cols) as u16,
                pixel_height: (cell_height * rows) as u16,
            })
            .context("opening pty for background command")?;

        let child = pair
            .slave
            .spawn_command(builder)
            .context("spawning background command")?;
        // The parent doesn't need the slave side once the child is
        // attached to it; holding it open would prevent the master side
        // from seeing EOF when the child exits.
        drop(pair.slave);

        let term_size = TerminalSize {
            rows,
            cols,
            pixel_width: cell_width as usize * cols,
            pixel_height: cell_height as usize * rows,
            dpi: 0,
        };

        // `Terminal` is explicitly designed to run without a real pty or
        // gui attached (see term/src/lib.rs's crate doc); the writer here
        // only ever receives keyboard/mouse-encoding "answerback" bytes
        // that we have no reason to send anywhere for a one-way background
        // effect, so a plain discarding sink is correct.
        let terminal = Terminal::new(
            term_size,
            Arc::new(TermConfig::new()),
            "WezTerm",
            config::wezterm_version(),
            Box::new(Vec::new()),
        );
        let terminal = Arc::new(Mutex::new(terminal));

        let mut reader = pair
            .master
            .try_clone_reader()
            .context("cloning background command pty reader")?;
        let term_for_thread = Arc::clone(&terminal);
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        term_for_thread.lock().unwrap().advance_bytes(&buf[..n]);
                    }
                }
            }
        });

        let glyph_cache = GlyphCache::new_in_memory(&fonts, 256)?;

        // 1x1 fully transparent placeholder; overwritten on the first
        // `get_frame` call, which always considers itself due immediately
        // because `last_rendered` is set far in the past below.
        let last_image = Arc::new(ImageData::with_data(ImageDataType::new_single_frame(
            1,
            1,
            vec![0, 0, 0, 0],
        )));

        Ok(Self {
            inner: RefCell::new(Inner {
                child,
                _master: pair.master,
                terminal,
                glyph_cache,
                fonts,
                render_metrics,
                cols,
                rows,
                last_rendered: Instant::now() - Duration::from_secs(3600),
                last_image,
                prev_buf: Vec::new(),
                prev_dims: (0, 0),
                prev_cluster_hashes: Vec::new(),
            }),
        })
    }

    /// Returns the current frame, rendering a fresh one first if the
    /// interval implied by `fps` has elapsed, plus the instant at which
    /// the next frame should be considered due (the caller is expected to
    /// feed this into the same "please wake me up and repaint at this
    /// time" mechanism used for animated gif/png backgrounds).
    pub fn get_frame(&self, fps: f32, width: usize, height: usize) -> (Arc<ImageData>, Instant) {
        let mut inner = self.inner.borrow_mut();
        let period = Duration::from_secs_f32(1.0 / fps.max(1.0));
        let now = Instant::now();
        if now.saturating_duration_since(inner.last_rendered) >= period {
            match inner.render_frame(width, height) {
                Ok(data) => {
                    inner.last_image = Arc::new(ImageData::with_data(data));
                    inner.last_rendered = now;
                }
                Err(err) => {
                    log::warn!("background Command: failed to render a frame: {:#}", err);
                    // Back off so we don't retry (and spam the log) every
                    // single paint tick while broken.
                    inner.last_rendered = now;
                }
            }
        }
        let due = inner.last_rendered + period;
        (Arc::clone(&inner.last_image), due)
    }
}

impl Inner {
    fn render_frame(&mut self, width: usize, height: usize) -> anyhow::Result<ImageDataType> {
        let render_metrics = self.render_metrics;
        let config = config::configuration();
        let cell_width = render_metrics.cell_size.width as usize;
        let cell_height = render_metrics.cell_size.height as usize;
        if cell_width == 0 || cell_height == 0 {
            anyhow::bail!("cell size is zero");
        }

        let needed_len = width * height * 4;
        // If the size changed, there's no valid previous frame to diff
        // against or reuse pixels from -- reset both to a fresh, opaque
        // black canvas and let every row below be treated as changed.
        let same_dims = self.prev_dims == (width, height) && self.prev_buf.len() == needed_len;
        if !same_dims {
            self.prev_buf.clear();
            self.prev_buf.resize(needed_len, 0);
            for px in self.prev_buf.chunks_exact_mut(4) {
                px[3] = 0xff;
            }
            self.prev_cluster_hashes.clear();
            self.prev_dims = (width, height);
        }

        let palette: wezterm_term::color::ColorPalette = config.resolved_palette.clone().into();

        let lines = {
            let mut term = self.terminal.lock().unwrap();
            let screen = term.screen_mut();
            screen.lines_in_phys_range(0..screen.physical_rows)
        };

        let max_rows = height / cell_height + 1;
        if self.prev_cluster_hashes.len() < max_rows {
            self.prev_cluster_hashes.resize(max_rows, Vec::new());
        }

        for (row_idx, line) in lines.iter().enumerate().take(max_rows) {
            let clusters = line.cluster(None);
            let cell_top = row_idx * cell_height;
            let cell_bottom = (cell_top + cell_height).min(height);

            // Cloned up front (rather than held as a live borrow through
            // the loop below) so it doesn't fight the borrow checker over
            // the other `self.*` fields (fonts, glyph_cache, prev_buf)
            // used inside the same loop.
            let prev_hashes = self.prev_cluster_hashes[row_idx].clone();
            let mut new_hashes = Vec::with_capacity(clusters.len());

            for (cluster_idx, cluster) in clusters.iter().enumerate() {
                let cluster_hash = hash_cluster(cluster);
                new_hashes.push(cluster_hash);

                // Same content, in the same slot, as last frame: the
                // pixels already sitting in `prev_buf` for this cluster's
                // cell range are still correct, so skip re-filling its
                // background and re-shaping/re-rasterizing its glyphs
                // entirely. This is the actual work-skipping step; the
                // rest of the loop below only runs for clusters that
                // changed.
                if same_dims && prev_hashes.get(cluster_idx) == Some(&cluster_hash) {
                    continue;
                }

                let bg = palette.resolve_bg(cluster.attrs.background());
                let (r, g, b, _a) = bg.to_srgb_u8();
                let x0 = (cluster.first_cell_idx * cell_width).min(width);
                let x1 = ((cluster.first_cell_idx + cluster.width) * cell_width).min(width);
                fill_rect(
                    &mut self.prev_buf,
                    width,
                    x0,
                    cell_top,
                    x1,
                    cell_bottom,
                    height,
                    r,
                    g,
                    b,
                );

                let style = self.fonts.match_style(&config, &cluster.attrs);
                let font = match self.fonts.resolve_font(style) {
                    Ok(f) => f,
                    Err(_) => continue,
                };
                let presentation_width = PresentationWidth::with_cluster(cluster);
                let infos = match font.blocking_shape(
                    &cluster.text,
                    Some(cluster.presentation),
                    cluster.direction,
                    None,
                    Some(&presentation_width),
                ) {
                    Ok(i) => i,
                    Err(_) => continue,
                };

                let fg = palette.resolve_fg(cluster.attrs.foreground());
                let (fr, fg8, fb, _fa) = fg.to_srgb_u8();

                for info in &infos {
                    let cell_idx = cluster.byte_to_cell_idx(info.cluster as usize);
                    let cell_x = cell_idx * cell_width;

                    let glyph = match self.glyph_cache.cached_glyph(
                        info,
                        style,
                        false,
                        &font,
                        &render_metrics,
                        info.num_cells,
                    ) {
                        Ok(g) => g,
                        Err(_) => continue,
                    };

                    let Some(sprite) = &glyph.texture else {
                        continue;
                    };

                    let pos_x = cell_x as f32 + (glyph.x_offset + glyph.bearing_x).get() as f32;
                    let pos_y = cell_top as f32
                        + cell_height as f32
                        + (render_metrics.descender.get() as f32
                            - (glyph.y_offset + glyph.bearing_y).get() as f32);

                    blit_glyph(
                        &mut self.prev_buf,
                        width,
                        height,
                        sprite,
                        pos_x,
                        pos_y,
                        glyph.has_color,
                        fr,
                        fg8,
                        fb,
                    );
                }
            }

            self.prev_cluster_hashes[row_idx] = new_hashes;
        }

        Ok(ImageDataType::new_single_frame(
            width as u32,
            height as u32,
            self.prev_buf.clone(),
        ))
    }
}

fn hash_cluster(c: &termwiz::cellcluster::CellCluster) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    c.text.hash(&mut hasher);
    c.attrs.foreground().hash(&mut hasher);
    c.attrs.background().hash(&mut hasher);
    c.first_cell_idx.hash(&mut hasher);
    c.width.hash(&mut hasher);
    hasher.finish()
}

fn fill_rect(
    buf: &mut [u8],
    width: usize,
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
    height: usize,
    r: u8,
    g: u8,
    b: u8,
) {
    let x1 = x1.min(width);
    let y1 = y1.min(height);
    for y in y0..y1 {
        for x in x0..x1 {
            let idx = (y * width + x) * 4;
            buf[idx] = r;
            buf[idx + 1] = g;
            buf[idx + 2] = b;
            buf[idx + 3] = 0xff;
        }
    }
}

/// Blits a single rasterized glyph onto `buf`, alpha-blending it over
/// whatever is already there (the cell's background, filled beforehand).
/// Monochrome glyphs are tinted with the resolved foreground color; color
/// glyphs (emoji) are drawn using their own baked-in color, matching how
/// `has_color` is used to pick a shader path in the real GPU renderer.
fn blit_glyph(
    buf: &mut [u8],
    width: usize,
    height: usize,
    sprite: &Sprite,
    pos_x: f32,
    pos_y: f32,
    has_color: bool,
    fr: u8,
    fg: u8,
    fb: u8,
) {
    let Some(tex) = sprite.texture.downcast_ref::<ImageTexture>() else {
        return;
    };
    let coords = &sprite.coords;
    let gw = coords.width() as usize;
    let gh = coords.height() as usize;
    if gw == 0 || gh == 0 {
        return;
    }
    let img = tex.image.borrow();

    for row in 0..gh {
        let y = pos_y as isize + row as isize;
        if y < 0 || y as usize >= height {
            continue;
        }
        let pixels = img.horizontal_pixel_range(
            coords.min_x() as usize,
            coords.max_x() as usize,
            coords.min_y() as usize + row,
        );
        for (col, &px) in pixels.iter().enumerate() {
            let x = pos_x as isize + col as isize;
            if x < 0 || x as usize >= width {
                continue;
            }
            let px = u32::from_be(px);
            let (b8, g8, r8, a8) = (
                (px >> 8) as u8,
                (px >> 16) as u8,
                (px >> 24) as u8,
                (px & 0xff) as u8,
            );
            if a8 == 0 {
                continue;
            }
            let (sr, sg, sb) = if has_color { (r8, g8, b8) } else { (fr, fg, fb) };
            let idx = (y as usize * width + x as usize) * 4;
            let a = a8 as u32;
            let inv = 255 - a;
            buf[idx] = ((sr as u32 * a + buf[idx] as u32 * inv) / 255) as u8;
            buf[idx + 1] = ((sg as u32 * a + buf[idx + 1] as u32 * inv) / 255) as u8;
            buf[idx + 2] = ((sb as u32 * a + buf[idx + 2] as u32 * inv) / 255) as u8;
            buf[idx + 3] = 0xff;
        }
    }
}
