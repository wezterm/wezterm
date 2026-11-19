use wezterm_dynamic::{FromDynamic, ToDynamic};

#[derive(Debug, Default, Clone, ToDynamic, PartialEq, Eq, FromDynamic)]
pub enum WindowLevel {
    AlwaysOnBottom = -1,
    #[default]
    Normal = 0,
    AlwaysOnTop = 3,
}

/// Controls the light/dark appearance of the window itself,
/// independently of the system-wide appearance.
#[derive(Debug, Default, Copy, Clone, ToDynamic, PartialEq, Eq, FromDynamic)]
pub enum WindowAppearance {
    /// Follow the appearance reported by the system
    #[default]
    System,
    /// Always use a dark appearance
    ForcedDark,
    /// Always use a light appearance
    ForcedLight,
}
