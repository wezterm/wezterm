use std::collections::HashMap;
use std::sync::LazyLock;

pub static NERD_FONTS: LazyLock<HashMap<&'static str, char>> = LazyLock::new(build_map);

pub use crate::nerdfonts_data::NERD_FONT_GLYPHS;

fn build_map() -> HashMap<&'static str, char> {
    crate::nerdfonts_data::NERD_FONT_GLYPHS
        .iter()
        .map(|tuple| *tuple)
        .collect()
}
