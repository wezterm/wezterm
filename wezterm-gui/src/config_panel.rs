use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::{Command, Stdio};

const CONFIG_EXECUTABLE: &str = "wezterm-config.exe";

fn executable_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("WEZTERM_CONFIG_EXE").filter(|path| !path.is_empty()) {
        return Ok(path.into());
    }

    let current = std::env::current_exe().context("resolve wezterm-gui executable path")?;
    let directory = current
        .parent()
        .context("wezterm-gui executable has no parent directory")?;
    Ok(directory.join(CONFIG_EXECUTABLE))
}

pub fn launch() -> Result<()> {
    let executable = executable_path()?;
    anyhow::ensure!(
        executable.is_file(),
        "configuration panel is not installed at {}",
        executable.display()
    );

    Command::new(&executable)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("launch {}", executable.display()))?;
    Ok(())
}
