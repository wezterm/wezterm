/// Shared utilities for loading PEM-encoded certificates from the filesystem
use anyhow::Context;
use std::path::Path;

/// Load PEM data from a file
pub fn load_pem_file(path: &Path) -> anyhow::Result<Vec<u8>> {
    std::fs::read(path).context(format!("reading PEM file from {}", path.display()))
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
    for root_cert_path in pem_root_certs {
        if root_cert_path.is_dir() {
            // If it's a directory, load all .pem files
            match std::fs::read_dir(root_cert_path) {
                Ok(entries) => {
                    for entry in entries {
                        if let Ok(entry) = entry {
                            let path = entry.path();
                            if path.extension().map_or(false, |ext| ext == "pem") {
                                match load_pem_file(&path) {
                                    Ok(data) => {
                                        if let Err(e) = f(data) {
                                            log::warn!(
                                                "Failed to process cert file {}: {}",
                                                path.display(),
                                                e
                                            );
                                        }
                                    }
                                    Err(e) => log::warn!(
                                        "Failed to read cert file {}: {}",
                                        path.display(),
                                        e
                                    ),
                                }
                            }
                        }
                    }
                }
                Err(e) => log::warn!(
                    "Failed to read directory {}: {}",
                    root_cert_path.display(),
                    e
                ),
            }
        } else {
            // If it's a file, load it directly
            match load_pem_file(root_cert_path) {
                Ok(data) => {
                    if let Err(e) = f(data) {
                        log::warn!(
                            "Failed to process cert file {}: {}",
                            root_cert_path.display(),
                            e
                        );
                    }
                }
                Err(e) => log::warn!(
                    "Failed to read cert file {}: {}",
                    root_cert_path.display(),
                    e
                ),
            }
        }
    }
    Ok(())
}
