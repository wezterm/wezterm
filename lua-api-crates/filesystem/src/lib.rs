use anyhow::anyhow;
use config::lua::mlua::{self, Lua};
use config::lua::{get_or_create_module, get_or_create_sub_module};
use config::DroppedFileQuoting;
use smol::prelude::*;

pub fn register(lua: &Lua) -> anyhow::Result<()> {
    // keep original module name for compatibility
    let wezterm_mod = get_or_create_module(lua, "wezterm")?;
    wezterm_mod.set("read_dir", lua.create_async_function(read_dir)?)?;
    wezterm_mod.set("glob", lua.create_async_function(glob)?)?;

    // Create a submodule for filesystem operations include old and new functions
    let wezterm_mod = get_or_create_sub_module(lua, "filesystem")?;
    wezterm_mod.set("read_dir", lua.create_async_function(read_dir)?)?;
    wezterm_mod.set("glob", lua.create_async_function(glob)?)?;
    wezterm_mod.set(
        "canonicalize_path",
        lua.create_function(|_, path: String| {
            let path = std::fs::canonicalize(path)?;
            let path = path.to_string_lossy().to_string();
            Ok(path)
        })?,
    )?;
    wezterm_mod.set(
        "dirname",
        lua.create_function(|_, path: String| {
            let path = std::path::Path::new(&path);
            let path = path.parent().unwrap_or(path);
            let path = path.to_string_lossy().to_string();
            Ok(path)
        })?,
    )?;
    wezterm_mod.set(
        "basename",
        lua.create_function(|_, path: String| {
            let path = std::path::Path::new(&path);
            let path = path.file_name().unwrap_or(path.as_ref());
            let path = path.to_string_lossy().to_string();
            Ok(path)
        })?,
    )?;
    wezterm_mod.set(
        "is_absolute_path",
        lua.create_function(|_, path: String| {
            let path = std::path::Path::new(&path);
            let is_absolute = path.is_absolute();
            Ok(is_absolute)
        })?,
    )?;
    wezterm_mod.set(
        "is_dir",
        lua.create_function(|_, path: String| {
            let path = std::fs::metadata(path)?;
            let is_dir = path.is_dir();
            Ok(is_dir)
        })?,
    )?;
    wezterm_mod.set(
        "is_file",
        lua.create_function(|_, path: String| {
            let path = std::fs::metadata(path)?;
            let is_file = path.is_file();
            Ok(is_file)
        })?,
    )?;
    wezterm_mod.set(
        "is_symlink",
        lua.create_function(|_, path: String| {
            let path = std::fs::symlink_metadata(path)?;
            let is_symlink = path.file_type().is_symlink();
            Ok(is_symlink)
        })?,
    )?;
    wezterm_mod.set(
        "exists",
        lua.create_function(|_, path: String| {
            let exists = std::path::Path::new(&path).exists();
            Ok(exists)
        })?,
    )?;
    wezterm_mod.set(
        "size",
        lua.create_function(|_, path: String| {
            let size = std::fs::metadata(path)?.len();
            Ok(size)
        })?,
    )?;
    wezterm_mod.set(
        "quote_path",
        lua.create_function(|_, (s, quoting): (String, DroppedFileQuoting)| {
            let result = quoting.escape(&s);
            Ok(result)
        })?,
    )?;

    Ok(())
}

async fn read_dir<'lua>(_: &'lua Lua, path: String) -> mlua::Result<Vec<String>> {
    let mut dir = smol::fs::read_dir(path)
        .await
        .map_err(mlua::Error::external)?;
    let mut entries = vec![];
    while let Some(entry) = dir.next().await {
        let entry = entry.map_err(mlua::Error::external)?;
        if let Some(utf8) = entry.path().to_str() {
            entries.push(utf8.to_string());
        } else {
            return Err(mlua::Error::external(anyhow!(
                "path entry {} is not representable as utf8",
                entry.path().display()
            )));
        }
    }
    Ok(entries)
}

async fn glob<'lua>(
    _: &'lua Lua,
    (pattern, path): (String, Option<String>),
) -> mlua::Result<Vec<String>> {
    let entries = smol::unblock(move || {
        let mut entries = vec![];
        let glob = filenamegen::Glob::new(&pattern)?;
        for path in glob.walk(path.as_deref().unwrap_or(".")) {
            if let Some(utf8) = path.to_str() {
                entries.push(utf8.to_string());
            } else {
                return Err(anyhow!(
                    "path entry {} is not representable as utf8",
                    path.display()
                ));
            }
        }
        Ok(entries)
    })
    .await
    .map_err(mlua::Error::external)?;
    Ok(entries)
}
