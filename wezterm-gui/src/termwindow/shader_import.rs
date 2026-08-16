use config::ImportedShaderPathBuf;
use std::path::Path;

use crate::termwindow::webgpu::ResolvedShader;

/// Errors that can occur during shader import (cross-compilation from
/// a foreign format to WGSL).
#[derive(Debug, thiserror::Error)]
pub enum ShaderImportError {
    #[error("failed to read shader file {path}: {error}")]
    ReadError {
        path: String,
        error: std::io::Error,
    },
    #[error("shader file {path} is not valid UTF-8: {error}")]
    InvalidUtf8 {
        path: String,
        error: std::str::Utf8Error,
    },
    #[error("shader file {path} is empty")]
    EmptyShader { path: String },
}

/// Import (cross-compile) a foreign shader to a resolved WGSL shader.
pub fn import_shader(shader: &ImportedShaderPathBuf) -> Result<ResolvedShader, ShaderImportError> {
    match shader {
        ImportedShaderPathBuf::Ghostty(path) => import_ghostty(path.as_path()),
    }
}

fn import_ghostty(path: &Path) -> Result<ResolvedShader, ShaderImportError> {
    let path_str = path.display().to_string();

    let raw_bytes = std::fs::read(path).map_err(|e| ShaderImportError::ReadError {
        path: path_str.clone(),
        error: e,
    })?;

    // Skip a potential BOM that Windows software may have placed in the file.
    let source_str = std::str::from_utf8(&raw_bytes)
        .map_err(|e| ShaderImportError::InvalidUtf8 {
            path: path_str.clone(),
            error: e,
        })?
        .trim_start_matches('\u{FEFF}');

    if source_str.trim().is_empty() {
        return Err(ShaderImportError::EmptyShader {
            path: path_str,
        });
    }

    compile_ghostty(source_str, &path_str)
}

fn compile_ghostty(
    _shader_source: &str,
    _source_label: &str,
) -> Result<ResolvedShader, ShaderImportError> {
    todo!("ghostty shader cross-compilation is not yet implemented")
}
