use std::collections::VecDeque;
use std::time::{Duration, Instant};

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
    pub current_left: f32,
    pub current_top: f32,
    pub current_right: f32,
    pub current_bottom: f32,

    pub target_left: f32,
    pub target_top: f32,
    pub target_right: f32,
    pub target_bottom: f32,

    // Trailing history nodes
    pub trail: VecDeque<TrailNode>,

    // Kitty-style 4 trailing corners:
    // Index 0: Top-Left
    // Index 1: Top-Right
    // Index 2: Bottom-Right
    // Index 3: Bottom-Left
    pub corner_x: [f32; 4],
    pub corner_y: [f32; 4],

    pub last_update: Instant,
    pub is_animating: bool,
    pub initialized: bool,
}

impl CursorTrailState {
    pub fn new() -> Self {
        Self {
            current_left: 0.0,
            current_top: 0.0,
            current_right: 0.0,
            current_bottom: 0.0,
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
        snap: bool,
    ) {
        if !self.initialized || snap {
            self.current_left = left;
            self.current_top = top;
            self.current_right = right;
            self.current_bottom = bottom;
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

        // If target changed significantly:
        if (self.target_left - left).abs() > 0.1
            || (self.target_top - top).abs() > 0.1
            || (self.target_right - right).abs() > 0.1
            || (self.target_bottom - bottom).abs() > 0.1
        {
            let dist_x = (self.target_left - left).abs();
            let dist_y = (self.target_top - top).abs();

            // If jumped across a massive distance (e.g. window resize or clear screen), snap directly
            if dist_x > 700.0 || dist_y > 700.0 {
                self.current_left = left;
                self.current_top = top;
                self.current_right = right;
                self.current_bottom = bottom;
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

            // Save historical node for fading trail comet tail
            self.trail.push_back(TrailNode {
                left: self.current_left,
                top: self.current_top,
                right: self.current_right,
                bottom: self.current_bottom,
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
    }

    /// Advance physics by delta time. Returns true if animation is in progress.
    pub fn tick(&mut self, now: Instant, decay_secs: f32) -> bool {
        if !self.initialized {
            return false;
        }

        let dt = now
            .duration_since(self.last_update)
            .as_secs_f32()
            .clamp(0.001, 0.05);
        self.last_update = now;

        let target_corners_x = [
            self.target_left,
            self.target_right,
            self.target_right,
            self.target_left,
        ];
        let target_corners_y = [
            self.target_top,
            self.target_top,
            self.target_bottom,
            self.target_bottom,
        ];

        let decay_fast = (decay_secs * 0.45).max(0.03);
        let decay_slow = decay_secs.max(0.09);

        let center_x = (self.target_left + self.target_right) * 0.5;
        let center_y = (self.target_top + self.target_bottom) * 0.5;
        let current_center_x = (self.current_left + self.current_right) * 0.5;
        let current_center_y = (self.current_top + self.current_bottom) * 0.5;

        let motion_dx = center_x - current_center_x;
        let motion_dy = center_y - current_center_y;
        let motion_len = (motion_dx * motion_dx + motion_dy * motion_dy).sqrt();

        // 1. Move each corner towards its target corner using Kitty's exponential ease
        for i in 0..4 {
            let dx = target_corners_x[i] - self.corner_x[i];
            let dy = target_corners_y[i] - self.corner_y[i];
            let dist = (dx * dx + dy * dy).sqrt();

            if dist < 0.25 {
                self.corner_x[i] = target_corners_x[i];
                self.corner_y[i] = target_corners_y[i];
                continue;
            }

            // Dot product with motion vector: leading corner moves fast, trailing corner lags
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

        // 2. Smoothly lerp current bounds towards target
        let lerp_speed = 32.0;
        let lerp_factor = 1.0 - (-lerp_speed * dt).exp();
        self.current_left += (self.target_left - self.current_left) * lerp_factor;
        self.current_top += (self.target_top - self.current_top) * lerp_factor;
        self.current_right += (self.target_right - self.current_right) * lerp_factor;
        self.current_bottom += (self.target_bottom - self.current_bottom) * lerp_factor;

        // 3. Prune old trail nodes
        let trail_lifetime = Duration::from_secs_f32(decay_slow * 1.6);
        self.trail
            .retain(|pt| now.duration_since(pt.time) < trail_lifetime);

        // Check arrival threshold
        let mut all_arrived = true;
        for i in 0..4 {
            if (self.corner_x[i] - target_corners_x[i]).abs() >= 0.4
                || (self.corner_y[i] - target_corners_y[i]).abs() >= 0.4
            {
                all_arrived = false;
                break;
            }
        }
        if (self.current_left - self.target_left).abs() >= 0.4
            || (self.current_top - self.target_top).abs() >= 0.4
        {
            all_arrived = false;
        }

        if all_arrived && self.trail.is_empty() {
            self.current_left = self.target_left;
            self.current_top = self.target_top;
            self.current_right = self.target_right;
            self.current_bottom = self.target_bottom;
            self.corner_x = target_corners_x;
            self.corner_y = target_corners_y;
            self.is_animating = false;
            false
        } else {
            self.is_animating = true;
            true
        }
    }
}
