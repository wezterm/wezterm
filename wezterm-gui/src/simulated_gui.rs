//! A simulated GUI environment for `wezterm check-config`.
//!
//! `check-config` evaluates the configuration without a window system, so
//! we mock `wezterm.gui` functions that can be potentially needed for this check.

use config::lua::get_or_create_sub_module;
use config::lua::mlua::Lua;
use std::collections::HashMap;
use std::sync::Mutex;
use wezterm_gui_subcommands::CheckAppearance;
use window_funcs::{ScreenInfo, Screens};

static SIMULATED_APPEARANCE: Mutex<CheckAppearance> = Mutex::new(CheckAppearance::Light);

pub const SIMULATED_SCREEN_NAME: &str = "simulated";

/// Must be called before the configuration is loaded.
pub fn set_simulated_appearance(appearance: CheckAppearance) {
    *SIMULATED_APPEARANCE.lock().unwrap() = appearance;
}

/// One ordinary non-HiDPI display: 1920x1080 pixels at the origin.
pub fn simulated_screens() -> Screens {
    let screen = ScreenInfo {
        name: SIMULATED_SCREEN_NAME.to_string(),
        x: 0,
        y: 0,
        width: 1920,
        height: 1080,
        scale: 1.0,
        max_fps: Some(60),
        effective_dpi: Some(96.0),
    };

    let mut by_name = HashMap::new();
    by_name.insert(screen.name.clone(), screen.clone());

    Screens {
        main: screen.clone(),
        active: screen,
        by_name,
        origin_x: 0,
        origin_y: 0,
        virtual_width: 1920,
        virtual_height: 1080,
    }
}

/// Register after `window_funcs::register`, which creates the entries this
/// overwrites: setup functions run in registration order.
pub fn register(lua: &Lua) -> anyhow::Result<()> {
    let gui = get_or_create_sub_module(lua, "gui")?;

    gui.set(
        "get_appearance",
        lua.create_function(|_, _: ()| Ok(SIMULATED_APPEARANCE.lock().unwrap().as_wezterm_name()))?,
    )?;

    gui.set(
        "screens",
        lua.create_function(|_, _: ()| Ok(simulated_screens()))?,
    )?;

    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn the_simulated_screen_is_self_consistent() {
        let screens = simulated_screens();

        assert_eq!(screens.main.name, SIMULATED_SCREEN_NAME);
        assert_eq!(screens.active.name, SIMULATED_SCREEN_NAME);
        assert!(screens.by_name.contains_key(SIMULATED_SCREEN_NAME));

        // The virtual desktop has to cover the screen inside it, or a
        // window positioned relative to the virtual origin lands off it.
        assert_eq!(screens.virtual_width, screens.main.width);
        assert_eq!(screens.virtual_height, screens.main.height);
        assert_eq!(screens.origin_x, screens.main.x);
        assert_eq!(screens.origin_y, screens.main.y);
    }

    #[test]
    fn the_simulated_screen_fills_in_its_optional_fields() {
        let screens = simulated_screens();

        assert!(screens.main.effective_dpi.is_some());
        assert!(screens.main.max_fps.is_some());
    }

    #[test]
    fn the_simulated_appearance_starts_light_and_can_be_set() {
        // The default is what a plain `check-config` reports.
        assert_eq!(
            *SIMULATED_APPEARANCE.lock().unwrap(),
            CheckAppearance::Light
        );

        set_simulated_appearance(CheckAppearance::Dark);
        assert_eq!(
            SIMULATED_APPEARANCE.lock().unwrap().as_wezterm_name(),
            "Dark"
        );

        set_simulated_appearance(CheckAppearance::Light);
    }
}
