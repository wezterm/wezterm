use crate::quad::{Quad, V_BOT_LEFT, V_BOT_RIGHT, V_TOP_LEFT, V_TOP_RIGHT};
use config::CursorTrailConfig;
use mux::renderable::StableCursorPosition;
use std::time::Instant;
use wezterm_term::StableRowIndex;

/// Distance threshold for considering corners "at cursor"
const SETTLED_THRESHOLD: f32 = 0.1;

/// A screen position in f32 coordinates
#[derive(Debug, Default, Copy, Clone, PartialEq)]
struct Pos {
    x: f32,
    y: f32,
}
impl From<StableCursorPosition> for Pos {
    fn from(p: StableCursorPosition) -> Self {
        Pos {
            x: p.x as f32,
            y: p.y as f32,
        }
    }
}

/// The vertices for the trail quad
#[derive(Debug, Default)]
struct TrailQuad([Pos; 4]);

impl std::ops::Index<usize> for TrailQuad {
    type Output = Pos;
    fn index(&self, idx: usize) -> &Self::Output {
        &self.0[idx]
    }
}

impl std::ops::IndexMut<usize> for TrailQuad {
    fn index_mut(&mut self, idx: usize) -> &mut Self::Output {
        &mut self.0[idx]
    }
}

impl TrailQuad {
    fn at(p: Pos) -> Self {
        Self([
            Pos { x: p.x, y: p.y },
            Pos {
                x: p.x + 1.0,
                y: p.y,
            },
            Pos {
                x: p.x + 1.0,
                y: p.y + 1.0,
            },
            Pos {
                x: p.x,
                y: p.y + 1.0,
            },
        ])
    }

    fn interp(&mut self, target: &TrailTarget, delta_time: f32, decay_fast: f32, decay_slow: f32) {
        let target_x = [target.left, target.right, target.right, target.left];
        let target_y = [target.top, target.top, target.bottom, target.bottom];

        let target_center_x = (target.left + target.right) * 0.5;
        let target_center_y = (target.top + target.bottom) * 0.5;
        let target_width = target.right - target.left;
        let target_height = target.bottom - target.top;
        let target_diag_2 = (target_width.powi(2) + target_height.powi(2)).sqrt() * 0.5;

        let mut dx = [0.0_f32; 4];
        let mut dy = [0.0_f32; 4];
        let mut dot = [0.0_f32; 4];

        for i in 0..4 {
            dx[i] = target_x[i] - self.0[i].x;
            dy[i] = target_y[i] - self.0[i].y;

            if dx[i].abs() < 1e-6 && dy[i].abs() < 1e-6 {
                dx[i] = 0.0;
                dy[i] = 0.0;
                dot[i] = 0.0;
            } else {
                let norm = (dx[i].powi(2) + dy[i].powi(2)).sqrt();
                let corner_to_center_x = target_x[i] - target_center_x;
                let corner_to_center_y = target_y[i] - target_center_y;
                dot[i] = (dx[i] * corner_to_center_x + dy[i] * corner_to_center_y)
                    / (target_diag_2 * norm);
            }
        }

        let min_dot = dot.iter().cloned().fold(f32::INFINITY, f32::min);
        let max_dot = dot.iter().cloned().fold(f32::NEG_INFINITY, f32::max);

        for i in 0..4 {
            if (dx[i] == 0.0 && dy[i] == 0.0) || min_dot.is_infinite() {
                continue;
            }

            let decay = if (max_dot - min_dot).abs() < 1e-6 {
                decay_slow
            } else {
                decay_slow + (decay_fast - decay_slow) * (dot[i] - min_dot) / (max_dot - min_dot)
            };

            let step = 1.0 - 2.0_f32.powf(-10.0 * delta_time / decay);
            self.0[i].x += dx[i] * step;
            self.0[i].y += dy[i] * step;
        }
    }
}

/// The edges to animate a TrailQuad towards
#[derive(Debug, Default)]
struct TrailTarget {
    top: f32,
    bottom: f32,
    left: f32,
    right: f32,
}
impl TrailTarget {
    fn at(p: Pos) -> Self {
        Self {
            top: p.y,
            bottom: p.y + 1.0,
            left: p.x,
            right: p.x + 1.0,
        }
    }
}

/// Info needed to update the CursorTrail state
pub struct TickContext {
    cursor_pos: Pos,
    now: Instant,
    distance_threshold: f32,
    decay_fast: f32,
    decay_slow: f32,
    dwell_treshold: u64,
}

impl TickContext {
    pub fn from_cursor(cursor_pos: StableCursorPosition, trail_config: &CursorTrailConfig) -> Self {
        let float_dur = trail_config.duration as f32;
        Self {
            cursor_pos: cursor_pos.into(),
            now: Instant::now(), // todo secs and such or take reference?
            distance_threshold: trail_config.distance_threshold as f32,
            decay_fast: float_dur / 1000.0,
            decay_slow: (float_dur * trail_config.spread) / 1000.0,
            dwell_treshold: trail_config.dwell_threshold,
        }
    }
}

/// Manages the cursor trail effect with a deformable quad
#[derive(Debug)]
pub struct CursorTrail {
    /// Four corners of the trail quad: top-left, top-right, bottom-right, bottom-left
    quad: TrailQuad,

    /// Trail target bounds (where corners are animating towards)
    // todo: structify
    target: TrailTarget,

    /// Last cursor position
    last_cursor_pos: Pos,

    /// When the cursor last moved to a new position
    cursor_last_moved: Instant,

    /// Timestamp of last update
    updated_at: Instant,
}

impl CursorTrail {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            quad: TrailQuad::default(),
            target: TrailTarget::default(),
            last_cursor_pos: Pos::default(),
            cursor_last_moved: now,
            updated_at: now,
        }
    }

    /// Update the trail state and return true if the quad should be rendered.
    pub fn tick(&mut self, ctx: TickContext) -> bool {
        let delta_time = ctx.now.duration_since(self.updated_at).as_secs_f32();
        self.updated_at = ctx.now;

        if self.last_cursor_pos != ctx.cursor_pos {
            self.cursor_last_moved = ctx.now;
            self.last_cursor_pos = ctx.cursor_pos;
        }

        if self.target.left == 0.0 && self.target.right == 0.0 {
            self.target = TrailTarget::at(ctx.cursor_pos);
            self.quad = TrailQuad::at(ctx.cursor_pos);
            return false;
        }

        let distance_to_cursor = (ctx.cursor_pos.x - self.target.left).abs()
            + (ctx.cursor_pos.y - self.target.top).abs();

        if distance_to_cursor > 0.0 && distance_to_cursor <= ctx.distance_threshold {
            self.target = TrailTarget::at(ctx.cursor_pos);
            self.quad = TrailQuad::at(ctx.cursor_pos);
            return false;
        }

        let dwell_time = ctx.now.duration_since(self.cursor_last_moved).as_millis() as u64;
        let dwelled = dwell_time >= ctx.dwell_treshold;

        if dwelled {
            self.target = TrailTarget::at(ctx.cursor_pos);
        }

        self.quad
            .interp(&self.target, delta_time, ctx.decay_fast, ctx.decay_slow);

        !self.settled(SETTLED_THRESHOLD) || !dwelled
    }

    fn settled(&self, threshold: f32) -> bool {
        for i in 0..4 {
            let target_x = if i == 1 || i == 2 {
                self.target.right
            } else {
                self.target.left
            };
            let target_y = if i >= 2 {
                self.target.bottom
            } else {
                self.target.top
            };
            let dx = target_x - self.quad[i].x;
            let dy = target_y - self.quad[i].y;
            if dx.abs() > threshold || dy.abs() > threshold {
                return false;
            }
        }
        true
    }

    pub fn apply_to_quad(
        &self,
        quad: &mut Quad,
        cell_width: f32,
        cell_height: f32,
        pane_left: usize,
        pane_top: StableRowIndex,
        px_x: f32,
        px_y: f32,
    ) {
        quad.vert[V_TOP_LEFT].position = [
            px_x + (self.quad[0].x - pane_left as f32) * cell_width,
            px_y + (self.quad[0].y - pane_top as f32) * cell_height,
        ];
        quad.vert[V_TOP_RIGHT].position = [
            px_x + (self.quad[1].x - pane_left as f32) * cell_width,
            px_y + (self.quad[1].y - pane_top as f32) * cell_height,
        ];
        quad.vert[V_BOT_LEFT].position = [
            px_x + (self.quad[3].x - pane_left as f32) * cell_width,
            px_y + (self.quad[3].y - pane_top as f32) * cell_height,
        ];
        quad.vert[V_BOT_RIGHT].position = [
            px_x + (self.quad[2].x - pane_left as f32) * cell_width,
            px_y + (self.quad[2].y - pane_top as f32) * cell_height,
        ];
    }
}
