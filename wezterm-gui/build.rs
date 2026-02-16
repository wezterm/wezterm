#[cfg(windows)]
fn copy_windows_asset_dir(
    src_dir: &std::path::Path,
    dest_dir: &std::path::Path,
) -> anyhow::Result<()> {
    use anyhow::Context as _;

    if !src_dir.is_dir() {
        return Ok(());
    }

    std::fs::create_dir_all(dest_dir)
        .context(format!("create directory {}", dest_dir.display()))?;

    for entry in std::fs::read_dir(src_dir).context(format!("read {}", src_dir.display()))? {
        let entry = entry.context(format!("read entry in {}", src_dir.display()))?;
        if !entry
            .file_type()
            .context(format!("stat {}", entry.path().display()))?
            .is_file()
        {
            continue;
        }

        let src_name = entry.path();
        let dest_name = dest_dir.join(entry.file_name());

        if !dest_name.exists() {
            std::fs::copy(&src_name, &dest_name).context(format!(
                "copy {} -> {}",
                src_name.display(),
                dest_name.display()
            ))?;
        }
    }

    Ok(())
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    #[cfg(windows)]
    {
        use std::io::Write;
        use std::path::Path;
        let profile = std::env::var("PROFILE").unwrap();
        let target = std::env::var("TARGET").unwrap();
        let repo_dir = std::env::current_dir()
            .ok()
            .and_then(|cwd| cwd.parent().map(|p| p.to_path_buf()))
            .unwrap();
        let exe_output_dir = repo_dir.join("target").join(profile);
        let windows_dir = repo_dir.join("assets").join("windows");

        let asset_arch = if target.contains("x86_64") {
            Some("x64")
        } else if target.contains("aarch64") {
            Some("arm64")
        } else {
            None
        };

        if let Some(asset_arch) = asset_arch {
            copy_windows_asset_dir(
                &windows_dir.join("conhost").join(asset_arch),
                &exe_output_dir,
            )
            .unwrap();
            copy_windows_asset_dir(&windows_dir.join("angle").join(asset_arch), &exe_output_dir)
                .unwrap();
            copy_windows_asset_dir(
                &windows_dir.join("mesa").join(asset_arch),
                &exe_output_dir.join("mesa"),
            )
            .unwrap();
        }

        // If a file named `.tag` is present, we'll take its contents for the
        // version number that we report in wezterm -h.
        let mut ci_tag = String::new();
        if let Ok(tag) = std::fs::read("../.tag") {
            if let Ok(s) = String::from_utf8(tag) {
                ci_tag = s.trim().to_string();
                println!("cargo:rerun-if-changed=../.tag");
            }
        }
        let version = if ci_tag.is_empty() {
            let mut cmd = std::process::Command::new("git");
            cmd.args(&[
                "-c",
                "core.abbrev=8",
                "show",
                "-s",
                "--format=%cd-%h",
                "--date=format:%Y%m%d-%H%M%S",
            ]);
            if let Ok(output) = cmd.output() {
                if output.status.success() {
                    String::from_utf8_lossy(&output.stdout).trim().to_owned()
                } else {
                    "UNKNOWN".to_owned()
                }
            } else {
                "UNKNOWN".to_owned()
            }
        } else {
            ci_tag
        };

        let rcfile_name = Path::new(&std::env::var_os("OUT_DIR").unwrap()).join("resource.rc");
        let mut rcfile = std::fs::File::create(&rcfile_name).unwrap();
        println!("cargo:rerun-if-changed=../assets/windows/terminal.ico");
        write!(
            rcfile,
            r#"
#include <winres.h>
// This ID is coupled with code in window/src/os/windows/window.rs
#define IDI_ICON 0x101
1 RT_MANIFEST "{win}\\manifest.manifest"
IDI_ICON ICON "{win}\\terminal.ico"
VS_VERSION_INFO VERSIONINFO
FILEVERSION     1,0,0,0
PRODUCTVERSION  1,0,0,0
FILEFLAGSMASK   VS_FFI_FILEFLAGSMASK
FILEFLAGS       0
FILEOS          VOS__WINDOWS32
FILETYPE        VFT_APP
FILESUBTYPE     VFT2_UNKNOWN
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904E4"
        BEGIN
            VALUE "CompanyName",      "Wez Furlong\0"
            VALUE "FileDescription",  "WezTerm - Wez's Terminal Emulator\0"
            VALUE "FileVersion",      "{version}\0"
            VALUE "LegalCopyright",   "Wez Furlong, MIT licensed\0"
            VALUE "InternalName",     "\0"
            VALUE "OriginalFilename", "\0"
            VALUE "ProductName",      "WezTerm\0"
            VALUE "ProductVersion",   "{version}\0"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x409, 1252
    END
END
"#,
            win = windows_dir.display().to_string().replace("\\", "\\\\"),
            version = version,
        )
        .unwrap();
        drop(rcfile);

        // Obtain MSVC environment so that the rc compiler can find the right headers.
        // https://github.com/nabijaczleweli/rust-embed-resource/issues/11#issuecomment-603655972
        if let Some(tool) = cc::windows_registry::find_tool(target.as_str(), "cl.exe") {
            for (key, value) in tool.env() {
                std::env::set_var(key, value);
            }
        }
        embed_resource::compile(rcfile_name);
    }

    #[cfg(target_os = "macos")]
    {
        use anyhow::Context as _;
        let profile = std::env::var("PROFILE").unwrap();
        let repo_dir = std::env::current_dir()
            .ok()
            .and_then(|cwd| cwd.parent().map(|p| p.to_path_buf()))
            .unwrap();

        // We need to copy the plist to avoid the UNUserNotificationCenter asserting
        // due to not finding the application bundle
        let src_plist = repo_dir
            .join("assets")
            .join("macos")
            .join("WezTerm.app")
            .join("Contents")
            .join("Info.plist");
        let build_target_dir = std::env::var("CARGO_TARGET_DIR")
            .and_then(|s| Ok(std::path::PathBuf::from(s)))
            .unwrap_or(repo_dir.join("target").join(profile));
        let dest_plist = build_target_dir.join("Info.plist");
        println!("cargo:rerun-if-changed=assets/macos/WezTerm.app/Contents/Info.plist");

        std::fs::copy(&src_plist, &dest_plist)
            .context(format!(
                "copy {} -> {}",
                src_plist.display(),
                dest_plist.display()
            ))
            .unwrap();
    }
}
