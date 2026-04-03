/// Shared utilities for loading PEM-encoded certificates from the filesystem
use std::fs;
use std::path::Path;

/// Load a single certificate file and invoke the callback
fn load_and_process_cert<F>(path: &Path, f: &mut F)
where
    F: FnMut(Vec<u8>) -> anyhow::Result<()>,
{
    match fs::read(path) {
        Ok(data) => {
            if let Err(e) = f(data) {
                log::warn!(
                    "Failed to process cert file {path}: {e}",
                    path = path.display()
                );
            }
        }
        Err(e) => log::warn!(
            "Failed to read cert file {path}: {e}",
            path = path.display()
        ),
    }
}

/// Process all .pem files in a directory
fn load_certs_from_dir<F>(dir: &Path, f: &mut F)
where
    F: FnMut(Vec<u8>) -> anyhow::Result<()>,
{
    match fs::read_dir(dir) {
        Ok(entries) => {
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                if path.extension().map_or(false, |ext| ext == "pem") {
                    load_and_process_cert(&path, f);
                }
            }
        }
        Err(e) => log::warn!("Failed to read directory {dir}: {e}", dir = dir.display()),
    }
}

/// Iterate over PEM root certificate paths and load their contents.
///
/// For each path in `pem_root_certs`:
/// - If it's a directory, load all .pem files from it
/// - If it's a file, load it directly
///
/// The callback `f` is called for each successfully loaded PEM data.
pub fn load_pem_root_certs<F>(pem_root_certs: &[std::path::PathBuf], mut f: F) -> anyhow::Result<()>
where
    F: FnMut(Vec<u8>) -> anyhow::Result<()>,
{
    for path in pem_root_certs {
        if path.is_dir() {
            load_certs_from_dir(path, &mut f);
        } else {
            load_and_process_cert(path, &mut f);
        }
    }
    Ok(())
}
