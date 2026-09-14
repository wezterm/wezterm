use std::collections::VecDeque;
use std::time::{Duration, Instant};

const CORNER_TL: usize = 0;
const CORNER_TR: usize = 1;
const CORNER_BR: usize = 2;
const CORNER_BL: usize = 3;

#[derive(Clone, Copy, Debug)]
pub struct TrailNode {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub time: Instant,
}

#[derive(Clone, Debug)]
pub struct CursorTrailState {
    pub target_left: f32,
    pub target_top: f32,
    pub target_right: f32,
    pub target_bottom: f32,

    pub trail: VecDeque<TrailNode>,

    pub corner_x: [f32; 4],
    pub corner_y: [f32; 4],

    last_update: Instant,
    pub is_animating: bool,
    initialized: bool,
}

impl Default for CursorTrailState {
    fn default() -> Self {
        Self::new()
    }
}

impl CursorTrailState {
    pub fn new() -> Self {
        Self {
            target_left: 0.0,
            target_top: 0.0,
            target_right: 0.0,
            target_bottom: 0.0,
            trail: VecDeque::new(),
            corner_x: [0.0; 4],
            corner_y: [0.0; 4],
            last_update: Instant::now(),
            is_animating: false,
            initialized: false,
        }
    }

    pub fn set_target(
        &mut self,
        left: f32,
        top: f32,
        right: f32,
        bottom: f32,
        max_snap_distance: f32,
    ) {
        if !self.initialized {
            self.target_left = left;
            self.target_top = top;
            self.target_right = right;
            self.target_bottom = bottom;
            self.corner_x = [left, right, right, left];
            self.corner_y = [top, top, bottom, bottom];
            self.trail.clear();
            self.initialized = true;
            self.is_animating = false;
            self.last_update = Instant::now();
            return;
        }

        if (self.target_left - left).abs() <= 0.1
            && (self.target_top - top).abs() <= 0.1
            && (self.target_right - right).abs() <= 0.1
            && (self.target_bottom - bottom).abs() <= 0.1
        {
            return;
        }

        let dist_x = (self.target_left - left).abs();
        let dist_y = (self.target_top - top).abs();

        // Snap immediately on large jumps (such as screen clears or page scrolling)
        if dist_x > max_snap_distance || dist_y > max_snap_distance {
            self.target_left = left;
            self.target_top = top;
            self.target_right = right;
            self.target_bottom = bottom;
            self.corner_x = [left, right, right, left];
            self.corner_y = [top, top, bottom, bottom];
            self.trail.clear();
            self.is_animating = false;
            self.last_update = Instant::now();
            return;
        }

        let min_x = self.corner_x[CORNER_TL].min(self.corner_x[CORNER_BL]);
        let max_x = self.corner_x[CORNER_TR].max(self.corner_x[CORNER_BR]);
        let min_y = self.corner_y[CORNER_TL].min(self.corner_y[CORNER_TR]);
        let max_y = self.corner_y[CORNER_BL].max(self.corner_y[CORNER_BR]);

        self.trail.push_back(TrailNode {
            left: min_x,
            top: min_y,
            right: max_x,
            bottom: max_y,
            time: Instant::now(),
        });
        while self.trail.len() > 8 {
            self.trail.pop_front();
        }

        self.target_left = left;
        self.target_top = top;
        self.target_right = right;
        self.target_bottom = bottom;
        self.is_animating = true;
    }

    /// Advance physics by delta time. Returns true if animation is still active.
    pub fn tick(&mut self, now: Instant, decay_secs: f32) -> bool {
        if !self.initialized {
            return false;
        }

        let dt = now
            .duration_since(self.last_update)
            .as_secs_f32()
            .clamp(0.001, 0.05);
        self.last_update = now;

        let target_x = [
            self.target_left,
            self.target_right,
            self.target_right,
            self.target_left,
        ];
        let target_y = [
            self.target_top,
            self.target_top,
            self.target_bottom,
            self.target_bottom,
        ];

        let decay_fast = (decay_secs * 0.45).max(0.03);
        let decay_slow = decay_secs.max(0.09);

        let target_cx = (self.target_left + self.target_right) * 0.5;
        let target_cy = (self.target_top + self.target_bottom) * 0.5;
        let current_cx =
            (self.corner_x[0] + self.corner_x[1] + self.corner_x[2] + self.corner_x[3]) * 0.25;
        let current_cy =
            (self.corner_y[0] + self.corner_y[1] + self.corner_y[2] + self.corner_y[3]) * 0.25;

        let motion_dx = target_cx - current_cx;
        let motion_dy = target_cy - current_cy;
        let motion_len = (motion_dx * motion_dx + motion_dy * motion_dy).sqrt();

        // Move corners with directional exponential decay
        for i in 0..4 {
            let dx = target_x[i] - self.corner_x[i];
            let dy = target_y[i] - self.corner_y[i];
            let dist = (dx * dx + dy * dy).sqrt();

            if dist < 0.25 {
                self.corner_x[i] = target_x[i];
                self.corner_y[i] = target_y[i];
                continue;
            }

            let decay = if motion_len > 1.0 && dist > 1.0 {
                let dot = (dx * motion_dx + dy * motion_dy) / (dist * motion_len);
                if dot > 0.0 {
                    decay_fast
                } else {
                    decay_slow
                }
            } else {
                decay_fast
            };

            let step = 1.0 - (-10.0 * dt / decay).exp2();
            self.corner_x[i] += dx * step;
            self.corner_y[i] += dy * step;
        }

        let trail_lifetime = Duration::from_secs_f32(decay_slow * 1.6);
        self.trail
            .retain(|pt| now.duration_since(pt.time) < trail_lifetime);

        let mut all_arrived = true;
        for i in 0..4 {
            if (self.corner_x[i] - target_x[i]).abs() >= 0.4
                || (self.corner_y[i] - target_y[i]).abs() >= 0.4
            {
                all_arrived = false;
                break;
            }
        }

        if all_arrived && self.trail.is_empty() {
            self.corner_x = target_x;
            self.corner_y = target_y;
            self.is_animating = false;
            false
        } else {
            self.is_animating = true;
            true
        }
    }
}
