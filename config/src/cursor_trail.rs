use wezterm_dynamic::{FromDynamic, ToDynamic};

/// Configuration for cursor trail effect
#[derive(Debug, Clone, FromDynamic, ToDynamic)]
pub struct CursorTrailConfig {
    /// Enable cursor trail effect
    #[dynamic(default)]
    pub enabled: bool,

    /// Cursor trail dwell time in milliseconds.
    /// Cursor needs to be stationary for this amount of time before the trail will be drawn.
    #[dynamic(default = "default_dwell_threshold")]
    pub dwell_threshold: u64,

    /// Animation duration in milliseconds for leading edge corners to reach the cursor.
    #[dynamic(default = "default_duration")]
    pub duration: u64,

    /// Duration multiplier for trailing edge (trailing_duration = duration * spread)
    /// Higher values create more stretch/smear effect as trailing edges take longer.
    /// Must be > 1.0 (trailing edge must be slower than leading edge).
    #[dynamic(default = "default_spread")]
    pub spread: f32,

    /// Minimum distance (in cells) to trigger cursor trail
    #[dynamic(default = "default_distance_threshold")]
    pub distance_threshold: usize,

    /// Opacity for cursor trail (0.0 to 1.0)
    #[dynamic(default = "default_opacity")]
    pub opacity: f32,
}

impl CursorTrailConfig {
    /// Validates the configuration values
    pub fn validate(&self) -> Result<(), String> {
        if self.spread <= 1.0 {
            return Err(format!(
                "cursor_trail.spread must be > 1.0 (got {}). \
                 Trailing edge must be slower than leading edge for proper smear effect.",
                self.spread
            ));
        }
        if self.opacity < 0.0 || self.opacity > 1.0 {
            return Err(format!(
                "cursor_trail.opacity must be between 0.0 and 1.0 (got {})",
                self.opacity
            ));
        }
        Ok(())
    }
}

impl Default for CursorTrailConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            dwell_threshold: default_dwell_threshold(),
            duration: default_duration(),
            spread: default_spread(),
            distance_threshold: default_distance_threshold(),
            opacity: default_opacity(),
        }
    }
}

fn default_duration() -> u64 {
    100 // milliseconds - leading edges reach ~99.9% of distance to target
}

fn default_spread() -> f32 {
    4.0 // trailing edge duration = duration * spread (400ms with default duration)
}

fn default_distance_threshold() -> usize {
    2 // cells
}

fn default_dwell_threshold() -> u64 {
    50 // milliseconds
}

fn default_opacity() -> f32 {
    0.8
}
