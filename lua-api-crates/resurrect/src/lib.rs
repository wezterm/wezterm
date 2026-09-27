//! Bundles the resurrect.wezterm plugin into wezterm.
//!
//! The Lua sources of <https://github.com/MLFlexer/resurrect.wezterm> are
//! compiled into the binary and registered in `package.preload`, so that
//! `require("resurrect")` works without fetching anything from the network.
//! The helpers that the upstream plugin implemented by spawning external
//! processes (`mkdir`, `find`, VBS scripts via `wscript.exe`) are provided
//! natively by the `resurrect.native` module.
use config::lua::mlua::{self, Lua, MultiValue, Table, Value};
use std::path::Path;
use std::time::UNIX_EPOCH;

/// (module name, chunk name, source) of the bundled Lua modules
const MODULES: &[(&str, &str, &str)] = &[
    (
        "resurrect",
        "resurrect/init.lua",
        include_str!("lua/init.lua"),
    ),
    (
        "resurrect.encryption",
        "resurrect/encryption.lua",
        include_str!("lua/resurrect/encryption.lua"),
    ),
    (
        "resurrect.file_io",
        "resurrect/file_io.lua",
        include_str!("lua/resurrect/file_io.lua"),
    ),
    (
        "resurrect.fuzzy_loader",
        "resurrect/fuzzy_loader.lua",
        include_str!("lua/resurrect/fuzzy_loader.lua"),
    ),
    (
        "resurrect.pane_tree",
        "resurrect/pane_tree.lua",
        include_str!("lua/resurrect/pane_tree.lua"),
    ),
    (
        "resurrect.state_manager",
        "resurrect/state_manager.lua",
        include_str!("lua/resurrect/state_manager.lua"),
    ),
    (
        "resurrect.tab_state",
        "resurrect/tab_state.lua",
        include_str!("lua/resurrect/tab_state.lua"),
    ),
    (
        "resurrect.utils",
        "resurrect/utils.lua",
        include_str!("lua/resurrect/utils.lua"),
    ),
    (
        "resurrect.window_state",
        "resurrect/window_state.lua",
        include_str!("lua/resurrect/window_state.lua"),
    ),
    (
        "resurrect.workspace_state",
        "resurrect/workspace_state.lua",
        include_str!("lua/resurrect/workspace_state.lua"),
    ),
];

pub fn register(lua: &Lua) -> anyhow::Result<()> {
    let package: Table = lua.globals().get("package")?;
    let preload: Table = package.get("preload")?;

    for &(name, chunk_name, source) in MODULES {
        let loader = lua.create_function(move |lua, _: MultiValue| {
            lua.load(source)
                .set_name(format!("@{chunk_name}"))
                .call::<_, Value>(())
        })?;
        preload.set(name, loader)?;
    }

    preload.set(
        "resurrect.native",
        lua.create_function(|lua, _: MultiValue| native_module(lua))?,
    )?;

    Ok(())
}

fn native_module(lua: &Lua) -> mlua::Result<Table<'_>> {
    let module = lua.create_table()?;
    module.set(
        "default_state_dir",
        lua.create_function(|_, ()| Ok(default_state_dir()))?,
    )?;
    module.set(
        "ensure_dir",
        lua.create_function(|_, path: String| {
            std::fs::create_dir_all(&path)
                .map_err(|err| mlua::Error::external(format!("creating {path}: {err:#}")))
        })?,
    )?;
    module.set(
        "list_json_files",
        lua.create_function(|_, path: String| Ok(list_json_files(Path::new(&path))))?,
    )?;
    Ok(module)
}

/// Returns the directory in which the state is saved by default.
/// The plugin expects it to end with a path separator.
fn default_state_dir() -> String {
    let mut dir = config::DATA_DIR
        .join("resurrect")
        .to_string_lossy()
        .into_owned();
    dir.push(std::path::MAIN_SEPARATOR);
    dir
}

/// Recursively lists the `.json` files below `base`, producing one
/// `<mtime in seconds since the unix epoch> <path>` line per file, which
/// is the format that the fuzzy loader parses.
/// Unreadable directories are skipped and symlinks are not followed.
fn list_json_files(base: &Path) -> String {
    fn walk(dir: &Path, lines: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if file_type.is_dir() {
                walk(&path, lines);
            } else if file_type.is_file()
                && path
                    .extension()
                    .map_or(false, |ext| ext.eq_ignore_ascii_case("json"))
            {
                let mtime = entry
                    .metadata()
                    .and_then(|meta| meta.modified())
                    .ok()
                    .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                    .map_or(0, |duration| duration.as_secs());
                lines.push(format!("{mtime} {}", path.display()));
            }
        }
    }

    let mut lines = vec![];
    walk(base, &mut lines);
    lines.sort();
    let mut output = lines.join("\n");
    if !output.is_empty() {
        output.push('\n');
    }
    output
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn modules_compile() {
        let lua = Lua::new();
        for &(name, chunk_name, source) in MODULES {
            lua.load(source)
                .set_name(chunk_name)
                .into_function()
                .unwrap_or_else(|err| panic!("{name}: {err:#}"));
        }
    }

    #[test]
    fn native_file_helpers() {
        let base = std::env::temp_dir().join(format!("resurrect-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);

        let lua = Lua::new();
        register(&lua).unwrap();
        lua.globals().set("base", base.to_string_lossy()).unwrap();
        lua.load(
            r#"
            local native = require("resurrect.native")
            native.ensure_dir(base .. "/workspace")
            native.ensure_dir(base .. "/tab/nested")
            -- already existing directories are fine
            native.ensure_dir(base .. "/workspace")
            assert(native.list_json_files(base) == "")
            assert(native.list_json_files(base .. "/does-not-exist") == "")
            "#,
        )
        .exec()
        .unwrap();

        std::fs::write(base.join("workspace").join("main.json"), "{}").unwrap();
        std::fs::write(base.join("tab").join("nested").join("x.JSON"), "{}").unwrap();
        std::fs::write(base.join("tab").join("notes.txt"), "").unwrap();

        let listing = list_json_files(&base);
        let files: Vec<&str> = listing
            .lines()
            .map(|line| {
                let (mtime, path) = line.split_once(' ').unwrap();
                assert!(mtime.parse::<u64>().unwrap() > 0, "{line}");
                path
            })
            .collect();
        assert_eq!(
            files,
            vec![
                base.join("tab")
                    .join("nested")
                    .join("x.JSON")
                    .display()
                    .to_string(),
                base.join("workspace")
                    .join("main.json")
                    .display()
                    .to_string(),
            ]
        );

        assert!(default_state_dir().ends_with(std::path::MAIN_SEPARATOR));
        std::fs::remove_dir_all(&base).unwrap();
    }

    /// Builds the pane tree that the plugin saves for a tab that has
    /// panes with the specified (left, top, width, height) and returns
    /// it as a string of the form "left,top[R:right][B:bottom]"
    fn pane_tree(panes: &[(usize, usize, usize, usize)]) -> String {
        let lua = Lua::new();
        register(&lua).unwrap();
        // Just enough of the wezterm module for pane_tree.lua
        lua.load(
            r#"
            package.loaded.wezterm = {
                target_triple = "x86_64-unknown-linux-gnu",
                log_warn = function() end,
                emit = function() end,
                mux = {
                    get_domain = function()
                        return { is_spawnable = function() return false end }
                    end,
                },
            }
            function describe(tree)
                if tree == nil then
                    return ""
                end
                local s = tree.left .. "," .. tree.top
                if tree.right then
                    s = s .. "[R:" .. describe(tree.right) .. "]"
                end
                if tree.bottom then
                    s = s .. "[B:" .. describe(tree.bottom) .. "]"
                end
                return s
            end
            "#,
        )
        .exec()
        .unwrap();

        let list = lua.create_table().unwrap();
        for (i, &(left, top, width, height)) in panes.iter().enumerate() {
            let pane: Table = lua
                .load(r#"{ get_domain_name = function() return "local" end }"#)
                .eval()
                .unwrap();
            let info = lua.create_table().unwrap();
            info.set("left", left).unwrap();
            info.set("top", top).unwrap();
            info.set("width", width).unwrap();
            info.set("height", height).unwrap();
            info.set("pane", pane).unwrap();
            list.set(i + 1, info).unwrap();
        }
        lua.globals().set("panes", list).unwrap();
        lua.load(r#"return describe(require("resurrect.pane_tree").create_pane_tree(panes))"#)
            .eval()
            .unwrap()
    }

    #[test]
    fn pane_tree_single_cell_dividers() {
        // Left half split top/bottom, right column: the layout from
        // mux's tab_splitting test with the default 1 cell dividers
        assert_eq!(
            pane_tree(&[(0, 0, 39, 11), (0, 12, 39, 12), (40, 0, 40, 24)]),
            "0,0[R:40,0][B:0,12]"
        );
    }

    #[test]
    fn pane_tree_wide_dividers() {
        // The same layout with pane_divider_cols = 3, pane_divider_rows = 2
        assert_eq!(
            pane_tree(&[(0, 0, 37, 10), (0, 12, 37, 12), (40, 0, 40, 24)]),
            "0,0[R:40,0][B:0,12]"
        );
        // Three columns, the middle one split top/bottom
        assert_eq!(
            pane_tree(&[
                (0, 0, 20, 24),
                (23, 0, 20, 10),
                (23, 12, 20, 12),
                (46, 0, 34, 24)
            ]),
            "0,0[R:23,0[R:46,0][B:23,12]]"
        );
    }
}
