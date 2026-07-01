use config::CursorTrailStyle;
use mux::renderable::StableCursorPosition;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use termwiz::surface::CursorShape;

// ── Performance cap ────────────────────────────────────────────────────────────
const MAX_PARTICLES: usize = 256;

// ── Fast PRNG ─────────────────────────────────────────────────────────────────
// xorshift64 with a global atomic counter as per-instance seed.

#[inline(always)]
fn ease_out_factor(rate: f32, dt: f32) -> f32 {
    let x = rate * dt;
    if x < 1.0 {
        let x2 = x * x;
        x - x2 * 0.5 + x2 * x * (1.0 / 6.0)
    } else {
        1.0 - (-x).exp()
    }
}

struct Rng(u64);

impl Rng {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0x517cc1b727220a95);
        // Fibonacci hashing increment keeps successive seeds well-distributed.
        let seed = COUNTER.fetch_add(0x9e3779b97f4a7c15, Ordering::Relaxed);
        Self(if seed == 0 { 1 } else { seed })
    }

    #[inline(always)]
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// Returns a value in [0, 1).
    #[inline(always)]
    fn next_f32(&mut self) -> f32 {
        // Use top 23 mantissa bits for uniform f32 in [0, 1)
        (self.next_u64() >> 41) as f32 * (1.0 / (1u64 << 23) as f32)
    }
}

// ── Neovide-style smear corners ───────────────────────────────────────────────
// Four independent corner animations (TL, TR, BR, BL).  Leading corners (most
// aligned with movement) get a faster speed so they race ahead; trailing corners
// use the full animation duration and lag behind.  The cursor body is then drawn
// as the free-form quad connecting the four animated positions — identical in
// principle to Neovide's PathBuilder(corners[0..3]) approach.

/// Fractional corner offsets from the cell top-left, in cell units.
/// Order: 0=TL, 1=TR, 2=BR, 3=BL — matching Neovide's STANDARD_CORNERS draw order.
pub const CORNER_OFFSETS: [(f32, f32); 4] = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];

/// Normalised direction from cell centre to each corner (used for alignment ranking).
/// centre is at (0.5, 0.5) in cell-relative coords; INV_SQRT2 ≈ 1/√2.
const CORNER_DIR: [(f32, f32); 4] = {
    const F: f32 = std::f32::consts::FRAC_1_SQRT_2;
    [(-F, -F), (F, -F), (F, F), (-F, F)]
};

/// Returns shape-adjusted corner offsets in cell units.
///
/// Matches Neovide's `set_cursor_shape` which adjusts `relative_position` per shape:
/// - **Block**: full cell corners `(0,0)→(1,1)`
/// - **Bar**: compressed horizontally to `cell_percentage` width, like Neovide's
///   `Vertical => ((x + 0.5) * cell_percentage - 0.5, y)`
/// - **Underline**: compressed vertically to the bottom `cell_percentage` of the cell,
///   like Neovide's `Horizontal => (x, -((-y + 0.5) * cell_percentage - 0.5))`
pub fn shape_corner_offsets(
    shape: CursorShape,
    cell_width: f32,
    cell_height: f32,
) -> [(f32, f32); 4] {
    const CELL_PERCENTAGE: f32 = 1.0 / 8.0;
    match shape {
        CursorShape::BlinkingBar | CursorShape::SteadyBar => {
            let bar_frac = (CELL_PERCENTAGE).max(2.0 / cell_width);
            [(0.0, 0.0), (bar_frac, 0.0), (bar_frac, 1.0), (0.0, 1.0)]
        }
        CursorShape::BlinkingUnderline | CursorShape::SteadyUnderline => {
            let ul_h = (cell_height * 0.1).max(2.0);
            let ul_frac = (ul_h / cell_height).min(1.0);
            let top_frac = 1.0 - ul_frac;
            [(0.0, top_frac), (1.0, top_frac), (1.0, 1.0), (0.0, 1.0)]
        }
        _ => CORNER_OFFSETS,
    }
}

/// One independently animated cursor corner for the Neovide-style deforming smear.
pub struct SmearCorner {
    /// Current rendered position in stable pane-relative pixel space:
    ///   x = column * cell_width,  y = row * cell_height
    pub visual_x: f32,
    pub visual_y: f32,
    initialized: bool,
    /// Exponential-ease speed for this corner; updated each time the cursor jumps.
    speed: f32,
}

impl SmearCorner {
    fn new() -> Self {
        SmearCorner {
            visual_x: 0.0,
            visual_y: 0.0,
            initialized: false,
            speed: 0.0,
        }
    }

    /// True while this corner hasn't yet reached its target position.
    pub fn is_animating(&self, target_x: f32, target_y: f32) -> bool {
        self.initialized
            && ((self.visual_x - target_x).abs() > 0.5 || (self.visual_y - target_y).abs() > 0.5)
    }

    fn snap_to(&mut self, x: f32, y: f32) {
        self.visual_x = x;
        self.visual_y = y;
        self.initialized = true;
    }

    fn tick(&mut self, target_x: f32, target_y: f32, dt: f32) {
        if !self.initialized || self.speed <= 0.0 {
            self.snap_to(target_x, target_y);
            return;
        }
        let factor = ease_out_factor(self.speed * 10.0, dt);
        self.visual_x += (target_x - self.visual_x) * factor;
        self.visual_y += (target_y - self.visual_y) * factor;
        if (self.visual_x - target_x).abs() < 0.5 {
            self.visual_x = target_x;
        }
        if (self.visual_y - target_y).abs() < 0.5 {
            self.visual_y = target_y;
        }
    }
}

// ── Particle ──────────────────────────────────────────────────────────────────

pub struct Particle {
    /// Position in stable pixel space:
    ///   x = column_in_pane * cell_width  (+half cell for centering)
    ///   y = stable_row     * cell_height (+half cell for centering)
    pub x: f32,
    pub y: f32,
    pub vx: f32,
    pub vy: f32,
    /// Angular velocity of the velocity vector (rad/sec).  Non-zero for Railgun.
    pub rotation_speed: f32,
    pub lifetime: f32,
    pub max_lifetime: f32,
}

// ── Highlight (single-point expanding effects) ────────────────────────────────

/// Which kind of expanding highlight to draw.
pub enum HighlightKind {
    /// Expanding filled square.
    SonicBoom { cx: f32, cy: f32 },
    /// Expanding hollow ring (four thin rectangles).
    Ripple { cx: f32, cy: f32 },
    /// Expanding hollow rectangle (cell aspect ratio).
    Wireframe {
        cx: f32,
        cy: f32,
        cell_w: f32,
        cell_h: f32,
    },
}

/// A single-point, time-driven highlight animation.
pub struct PointHighlight {
    /// Normalised progress: 0.0 (just born) → 1.0 (expired).
    pub t: f32,
    pub kind: HighlightKind,
}

// ── State ─────────────────────────────────────────────────────────────────────

pub struct CursorTrailState {
    pub particles: Vec<Particle>,
    pub highlight: Option<PointHighlight>,
    /// Four independently animated corners for Neovide-style deforming smear.
    /// Order: TL=0, TR=1, BR=2, BL=3.
    pub smear_corners: [SmearCorner; 4],
    /// Shape-adjusted corner offsets computed once per frame in update().
    /// Used by has_smear_animation() and paint_animated_cursor().
    cached_corner_offsets: [(f32, f32); 4],
    /// (cursor_shape, cell_width bits, cell_height bits) that produced
    /// `cached_corner_offsets`.  Re-computed only when this key changes.
    cached_corner_key: (CursorShape, u32, u32),
    prev_pos: StableCursorPosition,
    last_tick: Instant,
    /// Fractional carry so density is respected across frames.
    count_remainder: f32,
    rng: Rng,
    /// False until the first update(); skips spawning on the first frame
    /// to avoid a bogus trail from (0,0) to the real cursor position.
    prev_initialized: bool,
    /// A multi-cell jump that we've deferred for one or more update cycles
    /// to see whether the cursor immediately bounces back to its anchor.
    ///
    /// Apps like btop in tmux keep the cursor parked at a fixed cell and
    /// only briefly move it elsewhere during a redraw before snapping it
    /// back. If we armed the smear the moment we observed the jump, every
    /// such redraw would draw a flying cursor across the screen.
    ///
    /// Instead, on a multi-cell jump we record the source position here and
    /// freeze the smear corners. On each subsequent frame:
    ///   - if the cursor returns to `from`, we cancel the deferral (no smear);
    ///   - if the cursor stabilises elsewhere, or we've held longer than the
    ///     configured smear duration, we arm the smear belatedly;
    ///   - otherwise (cursor still moving multi-cell) we keep holding.
    ///
    /// The hold timeout is `cursor_animation_duration` itself: holding longer
    /// than the smear's own duration is pointless because the smear we'd
    /// eventually play would already have completed.
    pending_jump: Option<PendingJump>,
}

#[derive(Copy, Clone)]
struct PendingJump {
    from: StableCursorPosition,
    started_at: Instant,
}

impl CursorTrailState {
    pub fn new() -> Self {
        CursorTrailState {
            particles: Vec::new(),
            highlight: None,
            smear_corners: std::array::from_fn(|_| SmearCorner::new()),
            cached_corner_offsets: CORNER_OFFSETS,
            cached_corner_key: (CursorShape::Default, 0, 0),
            prev_pos: StableCursorPosition::default(),
            last_tick: Instant::now(),
            count_remainder: 0.0,
            rng: Rng::new(),
            prev_initialized: false,
            pending_jump: None,
        }
    }

    /// True while a multi-cell jump is being deferred to detect bouncing.
    /// During this state the smear corners are frozen at the pre-jump cell
    /// so the cursor visually appears to *not* have moved yet — the render
    /// path uses this to suppress the "cap" rectangle that would otherwise
    /// betray the deferred destination.
    #[inline]
    pub fn is_smear_deferred(&self) -> bool {
        self.pending_jump.is_some()
    }

    /// Returns true if there is anything to render this frame (particles / highlights).
    #[inline]
    pub fn has_active_animation(&self) -> bool {
        !self.particles.is_empty() || self.highlight.is_some()
    }

    /// Returns the shape-adjusted corner offsets cached during the last `update()`.
    #[inline]
    pub fn corner_offsets(&self) -> &[(f32, f32); 4] {
        &self.cached_corner_offsets
    }

    /// Returns true if any of the four smear corners haven't reached the target yet.
    /// Uses `cached_corner_offsets` computed during the last `update()`.
    pub fn has_smear_animation(
        &self,
        target_x: f32,
        target_y: f32,
        cell_width: f32,
        cell_height: f32,
    ) -> bool {
        let offsets = self.cached_corner_offsets;
        for (i, corner) in self.smear_corners.iter().enumerate() {
            let (ox, oy) = offsets[i];
            if corner.is_animating(target_x + ox * cell_width, target_y + oy * cell_height) {
                return true;
            }
        }
        false
    }

    /// Called when the cursor is hidden (`CursorVisibility::Hidden`).
    ///
    /// Silently tracks the new cursor position without spawning any effects and
    /// clears all in-flight animation state (particles, highlight, smear corners,
    /// pending jump) so that nothing is rendered while the cursor is invisible.
    ///
    /// `prev_initialized` is reset to `false` so that when the cursor becomes
    /// visible again it snaps immediately to its position rather than triggering
    /// a smear from the last known location.  This is O(1) — far cheaper than a
    /// full `update()` call.
    pub fn advance_hidden(&mut self, current: &StableCursorPosition) {
        self.prev_pos = *current;
        self.prev_initialized = false;
        self.particles.clear();
        self.highlight = None;
        self.pending_jump = None;
        for corner in self.smear_corners.iter_mut() {
            corner.initialized = false;
        }
    }

    /// Called once per frame from `paint_pane`.
    ///
    /// - Ticks existing particles / highlight forward by `dt`.
    /// - Spawns new particles / resets highlight if the cursor moved far enough.
    /// - Removes expired particles inline (O(n) swap-remove, no allocation).
    pub fn update(
        &mut self,
        current: &StableCursorPosition,
        cell_width: f32,
        cell_height: f32,
        min_distance: usize,
        style: Option<CursorTrailStyle>,
        density: f32,
        lifetime: f32,
        speed: f32,
        cursor_smear: bool,
        anim_length: f32,
        trail_size: f32,
        cursor_shape: CursorShape,
        now: Instant,
    ) {
        let dt = now.duration_since(self.last_tick).as_secs_f32().min(0.1);
        self.last_tick = now;

        let corner_key = (cursor_shape, cell_width.to_bits(), cell_height.to_bits());
        if corner_key != self.cached_corner_key {
            self.cached_corner_offsets = shape_corner_offsets(cursor_shape, cell_width, cell_height);
            self.cached_corner_key = corner_key;
        }

        let target_x = current.x as f32 * cell_width;
        let target_y = current.y as f32 * cell_height;
        let dx = (current.x as isize - self.prev_pos.x as isize).unsigned_abs();
        let dy = (current.y as isize - self.prev_pos.y as isize).unsigned_abs();

        if !self.prev_initialized {
            self.prev_pos = *current;
            self.prev_initialized = true;
            return;
        }

        // ── Tick particles ────────────────────────────────────────────────────
        // A tiny (rotation_speed, sin, cos) cache avoids recomputing sin_cos for
        // every particle.  Railgun particles all share rotation_speed = π, so
        // the cache turns ~256 sin_cos calls per frame into just one.
        let mut sincos_cache: (f32, f32, f32) = (f32::NAN, 0.0, 1.0);
        for p in &mut self.particles {
            p.lifetime -= dt;
            p.x += p.vx * dt;
            p.y += p.vy * dt;

            if p.rotation_speed != 0.0 {
                let (sin_a, cos_a) = if p.rotation_speed == sincos_cache.0 {
                    (sincos_cache.1, sincos_cache.2)
                } else {
                    let (s, c) = (p.rotation_speed * dt).sin_cos();
                    sincos_cache = (p.rotation_speed, s, c);
                    (s, c)
                };
                let vx = p.vx * cos_a - p.vy * sin_a;
                let vy = p.vx * sin_a + p.vy * cos_a;
                p.vx = vx;
                p.vy = vy;
            }
        }

        // O(1) swap-remove: dead particles replaced by the last element.
        let mut i = 0;
        while i < self.particles.len() {
            if self.particles[i].lifetime <= 0.0 {
                self.particles.swap_remove(i);
            } else {
                i += 1;
            }
        }
        if self.particles.is_empty() && self.particles.capacity() > MAX_PARTICLES * 2 {
            self.particles.shrink_to(MAX_PARTICLES);
        }

        // ── Tick highlight ────────────────────────────────────────────────────
        if let Some(ref mut h) = self.highlight {
            if lifetime > 0.0 {
                h.t += dt / lifetime;
            } else {
                h.t = 1.0;
            }
            if h.t >= 1.0 {
                self.highlight = None;
            }
        }

        // ── Spawn on movement ─────────────────────────────────────────────────
        let half_w = cell_width * 0.5;
        let half_h = cell_height * 0.5;

        match style {
            // No particle style configured: smear (if enabled) is the sole effect.
            None => {}

            // Particle styles: trigger when cursor moves at least min_distance cells.
            Some(
                inner @ (CursorTrailStyle::Railgun
                | CursorTrailStyle::Torpedo
                | CursorTrailStyle::PixieDust),
            ) => {
                if dx + dy >= min_distance {
                    let from_px = self.prev_pos.x as f32 * cell_width + half_w;
                    let from_py = self.prev_pos.y as f32 * cell_height + half_h;
                    let to_px = target_x + half_w;
                    let to_py = target_y + half_h;
                    let dist_x = to_px - from_px;
                    let dist_y = to_py - from_py;
                    let distance = dist_x.hypot(dist_y);
                    self.spawn_trail_particles(
                        from_px,
                        from_py,
                        dist_x,
                        dist_y,
                        distance,
                        cell_height,
                        density,
                        lifetime,
                        speed,
                        inner,
                    );
                }
            }

            // Highlight styles: trigger once per discrete cell move.
            Some(inner_style) => {
                if dx + dy >= min_distance {
                    let to_px = target_x + half_w;
                    let to_py = target_y + half_h;
                    match inner_style {
                        CursorTrailStyle::SonicBoom => {
                            self.highlight = Some(PointHighlight {
                                t: 0.0,
                                kind: HighlightKind::SonicBoom {
                                    cx: to_px,
                                    cy: to_py,
                                },
                            });
                        }
                        CursorTrailStyle::Ripple => {
                            self.highlight = Some(PointHighlight {
                                t: 0.0,
                                kind: HighlightKind::Ripple {
                                    cx: to_px,
                                    cy: to_py,
                                },
                            });
                        }
                        CursorTrailStyle::Wireframe => {
                            self.highlight = Some(PointHighlight {
                                t: 0.0,
                                kind: HighlightKind::Wireframe {
                                    cx: to_px,
                                    cy: to_py,
                                    cell_w: cell_width,
                                    cell_h: cell_height,
                                },
                            });
                        }
                        _ => {}
                    }
                }
            }
        }

        // ── Smear corners (Neovide-style 4-corner deforming smear) ───────────
        //
        // The interesting question is how to tell a real cursor jump (vim `G`,
        // user click) from a btop-in-tmux redraw bounce. Both look the same in
        // a single frame: the cursor moved several cells. The difference is
        // what happens *next*: a real jump stays put for a while, while btop's
        // redraw bounces the cursor back to its anchor within one or two
        // frames.
        //
        // So instead of arming the smear immediately, we *defer* the first
        // multi-cell jump for one update cycle. The corners are frozen at the
        // pre-jump cell. The render path uses `is_smear_deferred()` to also
        // suppress the cap rectangle, so the cursor visually appears to *not*
        // have moved yet. On the next call we know how to resolve:
        //
        //   • cursor returned to the anchor → the bounce was an artefact,
        //     cancel without ever drawing motion;
        //   • cursor stopped or shrank to a single-cell move (`dx + dy <= 1`)
        //     → arm the smear belatedly. Single-cell continuations are
        //     user-driven (typing, hjkl) and never part of a btop burst, so
        //     we treat them as confirming intent. Total latency in this path
        //     is one wezterm frame;
        //   • cursor still wandering multi-cell → keep holding (handles
        //     multi-frame bursts where tmux splits one btop redraw across
        //     several pushes);
        //   • we've held longer than `cursor_animation_duration` already →
        //     give up and arm. Holding longer than the smear's own duration
        //     makes no sense because the smear would already have completed.
        if cursor_smear {
            let offsets = self.cached_corner_offsets;

            #[derive(Copy, Clone)]
            enum Phase {
                Hold,
                Snap,
                Arm(StableCursorPosition),
                Tick,
            }

            let phase = if let Some(pending) = self.pending_jump {
                let returned = pending.from.x == current.x && pending.from.y == current.y;
                let timed_out = now.duration_since(pending.started_at).as_secs_f32() >= anim_length;
                if returned {
                    self.pending_jump = None;
                    Phase::Snap
                } else if timed_out || dx + dy <= 1 {
                    self.pending_jump = None;
                    Phase::Arm(pending.from)
                } else {
                    Phase::Hold
                }
            } else if dx + dy > 1 {
                // Transitioning into Hold for the first time: snap the corners
                // to the pre-jump cell. Without this, two edge cases would
                // make the cursor disappear or look distorted while held:
                //   • corners still uninitialized (e.g. just after enabling
                //     cursor_smear) → has_smear_animation() returns false, no
                //     quad drawn, and is_smear_deferred() suppresses the cap
                //     too — the cursor is invisible for the entire hold;
                //   • corners mid-animation from a previous smear → freezing
                //     them in flight leaves a distorted quad instead of a
                //     stable cursor block at the anchor.
                let from = self.prev_pos;
                self.pending_jump = Some(PendingJump {
                    from,
                    started_at: now,
                });
                let from_x = from.x as f32 * cell_width;
                let from_y = from.y as f32 * cell_height;
                for (i, corner) in self.smear_corners.iter_mut().enumerate() {
                    let (ox, oy) = offsets[i];
                    corner.snap_to(from_x + ox * cell_width, from_y + oy * cell_height);
                }
                Phase::Hold
            } else if dx + dy > 0 {
                Phase::Snap
            } else {
                Phase::Tick
            };

            // Compute per-corner speeds when arming from a deferred jump.
            // The corners are still sitting at `from` (we never ticked them
            // during the hold), so the smear naturally animates from there to
            // the current cell as the corners tick toward it below.
            if let Phase::Arm(from) = phase {
                let from_cx = from.x as f32 * cell_width + cell_width * 0.5;
                let from_cy = from.y as f32 * cell_height + cell_height * 0.5;
                let cur_cx = target_x + cell_width * 0.5;
                let cur_cy = target_y + cell_height * 0.5;
                let move_dx = cur_cx - from_cx;
                let move_dy = cur_cy - from_cy;
                let move_dist = move_dx.hypot(move_dy);

                if move_dist > 0.0 {
                    let travel_x = move_dx / move_dist;
                    let travel_y = move_dy / move_dist;

                    // alignment[i] = dot(corner_dir[i], travel_dir): +1 = fully leading.
                    let alignments: [f32; 4] = std::array::from_fn(|i| {
                        CORNER_DIR[i].0 * travel_x + CORNER_DIR[i].1 * travel_y
                    });

                    // Sort by descending alignment to assign ranks 0 (trailing) – 3 (leading).
                    let mut order = [0usize, 1, 2, 3];
                    order.sort_by(|&a, &b| {
                        alignments[b]
                            .partial_cmp(&alignments[a])
                            .unwrap_or(std::cmp::Ordering::Equal)
                    });
                    let mut ranks = [0usize; 4];
                    for (rank_from_back, &idx) in order.iter().rev().enumerate() {
                        ranks[idx] = rank_from_back;
                    }

                    // Duration per rank (matching Neovide's jump() logic).
                    let leading_dur = (anim_length * (1.0 - trail_size).max(0.05)).max(0.005);
                    let trailing_dur = anim_length.max(0.005);
                    for (i, corner) in self.smear_corners.iter_mut().enumerate() {
                        let dur = match ranks[i] {
                            3 | 2 => leading_dur,
                            1 => (leading_dur + trailing_dur) * 0.5,
                            _ => trailing_dur,
                        };
                        corner.speed = 0.3 / dur;
                    }
                }
            }

            match phase {
                Phase::Hold => {
                    // Corners frozen at the pre-jump position. The render path
                    // sees `is_smear_deferred() == true` and suppresses the cap
                    // rect, so visually the cursor appears unmoved.
                }
                Phase::Snap => {
                    for (i, corner) in self.smear_corners.iter_mut().enumerate() {
                        let (ox, oy) = offsets[i];
                        corner.snap_to(target_x + ox * cell_width, target_y + oy * cell_height);
                    }
                }
                Phase::Arm(_) | Phase::Tick => {
                    for (i, corner) in self.smear_corners.iter_mut().enumerate() {
                        let (ox, oy) = offsets[i];
                        corner.tick(target_x + ox * cell_width, target_y + oy * cell_height, dt);
                    }
                }
            }
        } else {
            // Smear disabled: mark corners uninitialized so re-enabling snaps them
            // to the correct target on the first tick, with no coordinate math here.
            for corner in self.smear_corners.iter_mut() {
                corner.initialized = false;
            }
            self.pending_jump = None;
        }

        self.prev_pos = *current;
    }

    // ── Particle spawning ─────────────────────────────────────────────────────

    fn spawn_trail_particles(
        &mut self,
        from_px: f32,
        from_py: f32,
        dist_x: f32,
        dist_y: f32,
        distance: f32,
        cell_height: f32,
        density: f32,
        lifetime: f32,
        speed: f32,
        style: CursorTrailStyle,
    ) {
        if distance < 0.1 {
            return;
        }

        // How many particles to spawn this movement.
        let raw = (distance / cell_height) * density + self.count_remainder;
        let count = raw.floor() as usize;
        self.count_remainder = raw - count as f32;

        // Never exceed the global cap.
        let spare = MAX_PARTICLES.saturating_sub(self.particles.len());
        let count = count.min(spare);
        if count == 0 {
            return;
        }

        let dir_x = dist_x / distance;
        let dir_y = dist_y / distance;
        // Perpendicular (90° counter-clockwise).
        let base_speed = cell_height * speed; // pixels / sec

        match style {
            CursorTrailStyle::Railgun => {
                use std::f32::consts::PI;
                // Neovide-matching Railgun:
                // - Particles placed on the straight travel path (no sinusoidal offset)
                // - Velocity fans out in screen-space via (sin, cos) of a phase that
                //   grows with position along the trail and travel distance
                // - Lifetime = t * max so near-origin particles are born dim and die
                //   fast, reproducing Neovide's characteristic fading-tail look
                // - Rotation: PI rad/sec (Neovide default vfx_particle_curl = 1.0)
                const PHASE_FACTOR: f32 = 1.5; // Neovide vfx_particle_phase default
                const CURL: f32 = 1.0; // Neovide vfx_particle_curl default
                for i in 0..count {
                    // t: 1/count → 1.0, matching Neovide's (i+1)/count indexing
                    let t = (i + 1) as f32 / count as f32;
                    // Spawn on the straight travel line
                    let px = from_px + dist_x * t;
                    let py = from_py + dist_y * t;
                    // Phase matches Neovide: t / PI * phase_factor * (distance / height)
                    let phase = t / PI * PHASE_FACTOR * (distance / cell_height);
                    // Velocity: rotate the travel-direction vector by -phase.
                    // This fans particles around the movement axis, so the effect
                    // looks consistent for both horizontal and vertical movement.
                    // For vertical-down (dir=(0,1)) this reduces to (sin,cos)*speed,
                    // exactly matching Neovide's screen-space formula.
                    let vx = (dir_x * phase.cos() + dir_y * phase.sin()) * base_speed * 2.0;
                    let vy = (-dir_x * phase.sin() + dir_y * phase.cos()) * base_speed * 2.0;
                    // lifetime = t * max so life_frac = t * remaining_fraction.
                    // Near-origin particles (small t) are born dim and die quickly.
                    self.particles.push(Particle {
                        x: px,
                        y: py,
                        vx,
                        vy,
                        rotation_speed: PI * CURL,
                        lifetime: t * lifetime,
                        max_lifetime: lifetime,
                    });
                }
            }
            CursorTrailStyle::Torpedo => {
                for i in 0..count {
                    let t = (i + 1) as f32 / count as f32;
                    let rand_t = self.rng.next_f32();
                    let px = from_px + dist_x * rand_t;
                    let py = from_py + dist_y * rand_t;
                    let rx = self.rng.next_f32() * 2.0 - 1.0;
                    let ry = self.rng.next_f32() * 2.0 - 1.0;
                    let rl = rx.hypot(ry).max(1e-6);
                    let rnx = rx / rl;
                    let rny = ry / rl;
                    let pdx = rnx - dir_x * 1.5;
                    let pdy = rny - dir_y * 1.5;
                    let pdl = pdx.hypot(pdy).max(1e-6);
                    let vx = pdx / pdl * base_speed;
                    let vy = pdy / pdl * base_speed;
                    let curl = (self.rng.next_f32() - 0.5) * std::f32::consts::FRAC_PI_2;
                    self.particles.push(Particle {
                        x: px,
                        y: py,
                        vx,
                        vy,
                        rotation_speed: curl,
                        lifetime: t * lifetime,
                        max_lifetime: lifetime,
                    });
                }
            }
            CursorTrailStyle::PixieDust => {
                for i in 0..count {
                    let t = (i + 1) as f32 / count as f32;
                    let rand_t = self.rng.next_f32();
                    let px = from_px + dist_x * rand_t;
                    let py = from_py + dist_y * rand_t;
                    let rx = self.rng.next_f32() * 2.0 - 1.0;
                    let ry = self.rng.next_f32() * 2.0 - 1.0;
                    let rl = rx.hypot(ry).max(1e-6);
                    let rnx = rx / rl;
                    let rny = ry / rl;
                    let vx = rnx * 0.5 * 3.0 * base_speed;
                    let vy = (0.4 + rny.abs()) * 3.0 * base_speed;
                    let curl = (self.rng.next_f32() - 0.5) * std::f32::consts::FRAC_PI_2;
                    self.particles.push(Particle {
                        x: px,
                        y: py,
                        vx,
                        vy,
                        rotation_speed: curl,
                        lifetime: t * lifetime,
                        max_lifetime: lifetime,
                    });
                }
            }
            _ => {}
        }
    }
}
