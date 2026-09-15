use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug)]
pub struct StreamSegment {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    pub time: Instant,
}

#[derive(Clone, Debug)]
pub struct CursorTrailState {
    pub current_x: f32,
    pub current_y: f32,
    pub target_x: f32,
    pub target_y: f32,

    pub stream: VecDeque<StreamSegment>,

    pub last_record_x: f32,
    pub last_record_y: f32,
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
            current_x: 0.0,
            current_y: 0.0,
            target_x: 0.0,
            target_y: 0.0,
            stream: VecDeque::new(),
            last_record_x: 0.0,
            last_record_y: 0.0,
            last_update: Instant::now(),
            is_animating: false,
            initialized: false,
        }
    }

    pub fn set_target(
        &mut self,
        target_x: f32,
        target_y: f32,
        max_snap_distance: f32,
    ) {
        if !self.initialized {
            self.current_x = target_x;
            self.current_y = target_y;
            self.target_x = target_x;
            self.target_y = target_y;
            self.last_record_x = target_x;
            self.last_record_y = target_y;
            self.stream.clear();
            self.initialized = true;
            self.is_animating = false;
            self.last_update = Instant::now();
            return;
        }

        let dist_x = (self.target_x - target_x).abs();
        let dist_y = (self.target_y - target_y).abs();

        // Snap immediately across large distances (screen clears, full-page scrolls)
        if dist_x > max_snap_distance || dist_y > max_snap_distance {
            self.current_x = target_x;
            self.current_y = target_y;
            self.target_x = target_x;
            self.target_y = target_y;
            self.last_record_x = target_x;
            self.last_record_y = target_y;
            self.stream.clear();
            self.is_animating = false;
            self.last_update = Instant::now();
            return;
        }

        if (self.target_x - target_x).abs() > 0.2 || (self.target_y - target_y).abs() > 0.2 {
            self.target_x = target_x;
            self.target_y = target_y;
            self.is_animating = true;
        }
    }

    /// Advance physics and update continuous stream trail. Returns true if animation is active.
    pub fn tick(&mut self, now: Instant, decay_secs: f32) -> bool {
        if !self.initialized {
            return false;
        }

        let dt = now
            .duration_since(self.last_update)
            .as_secs_f32()
            .clamp(0.001, 0.05);
        self.last_update = now;

        let dist_x = self.target_x - self.current_x;
        let dist_y = self.target_y - self.current_y;
        let dist = (dist_x * dist_x + dist_y * dist_y).sqrt();

        if dist > 0.2 {
            // Fluid Tron gliding ease with snappy responsiveness
            let speed = 32.0;
            let step = 1.0 - (-speed * dt).exp();
            self.current_x += dist_x * step;
            self.current_y += dist_y * step;
        } else {
            self.current_x = self.target_x;
            self.current_y = self.target_y;
        }

        // Record continuous stream segments without gaps
        let moved = ((self.current_x - self.last_record_x).powi(2)
            + (self.current_y - self.last_record_y).powi(2))
        .sqrt();

        let at_target = (self.current_x - self.target_x).abs() < 0.1
            && (self.current_y - self.target_y).abs() < 0.1;

        if moved >= 0.5 || (moved >= 0.1 && at_target) {
            self.stream.push_front(StreamSegment {
                x0: self.last_record_x,
                y0: self.last_record_y,
                x1: self.current_x,
                y1: self.current_y,
                time: now,
            });
            self.last_record_x = self.current_x;
            self.last_record_y = self.current_y;

            while self.stream.len() > 128 {
                self.stream.pop_back();
            }
        }

        // Prune old stream segments past decay duration
        let lifetime = Duration::from_secs_f32(decay_secs.max(0.08));
        self.stream
            .retain(|seg| now.duration_since(seg.time) < lifetime);

        if at_target && self.stream.is_empty() {
            self.current_x = self.target_x;
            self.current_y = self.target_y;
            self.last_record_x = self.target_x;
            self.last_record_y = self.target_y;
            self.is_animating = false;
            false
        } else {
            self.is_animating = true;
            true
        }
    }
}
