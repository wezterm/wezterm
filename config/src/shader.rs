use std::path::{Path, PathBuf};
use wezterm_dynamic::{FromDynamic, FromDynamicOptions, ToDynamic, Value};

/// Resolve a possibly-relative path against a base directory, leaving
/// absolute paths unchanged.
trait ResolveRelative {
    fn join_relative_to(&self, base: &Path) -> PathBuf;
}

impl ResolveRelative for PathBuf {
    fn join_relative_to(&self, base: &Path) -> PathBuf {
        if self.is_absolute() {
            self.clone()
        } else {
            base.join(self)
        }
    }
}

/// A shader path in the `custom_shaders` config list.
/// `Native` paths are WGSL and need no translation.
/// `Imported` paths are foreign formats that get cross-compiled to WGSL.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ShaderPathBuf {
    Native(PathBuf),
    Imported,
}

impl ShaderPathBuf {
    pub fn as_path(&self) -> &std::path::Path {
        match self {
            ShaderPathBuf::Native(p) => p,
            ShaderPathBuf::Imported => unreachable!("Imported shader paths carry no native path"),
        }
    }

    /// Return a new instance with the path resolved relative to `base`.
    pub fn join_relative_to(&self, base: &Path) -> Self {
        match self {
            ShaderPathBuf::Native(p) => ShaderPathBuf::Native(p.join_relative_to(base)),
            ShaderPathBuf::Imported => ShaderPathBuf::Imported,
        }
    }
}

impl FromDynamic for ShaderPathBuf {
    fn from_dynamic(
        value: &Value,
        _options: FromDynamicOptions,
    ) -> Result<Self, wezterm_dynamic::Error> {
        match value {
            // Bare string → native WGSL path
            Value::String(s) => Ok(ShaderPathBuf::Native(s.clone().into())),
            // Tagged object → imported shader
            Value::Object(_) => Ok(ShaderPathBuf::Imported),
            other => Err(wezterm_dynamic::Error::NoConversion {
                source_type: other.variant_name().to_string(),
                dest_type: "ShaderPathBuf",
            }),
        }
    }
}

impl ToDynamic for ShaderPathBuf {
    fn to_dynamic(&self) -> Value {
        match self {
            ShaderPathBuf::Native(p) => Value::String(p.to_string_lossy().to_string()),
            ShaderPathBuf::Imported => unreachable!("Imported shader paths have no serializable form"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bare_string_to_native() {
        let value = Value::String("/path/to/shader.wgsl".to_string());
        let shader = ShaderPathBuf::from_dynamic(&value, FromDynamicOptions::default()).unwrap();
        assert_eq!(shader, ShaderPathBuf::Native(PathBuf::from("/path/to/shader.wgsl")));
    }

    #[test]
    fn test_tagged_object_to_imported() {
        let mut obj = wezterm_dynamic::Object::default();
        obj.insert(Value::String("format".to_string()), Value::String("Ghostty".to_string()));
        obj.insert(Value::String("path".to_string()), Value::String("/path/to/crt.glsl".to_string()));
        let value = Value::Object(obj);
        let shader = ShaderPathBuf::from_dynamic(&value, FromDynamicOptions::default()).unwrap();
        assert_eq!(shader, ShaderPathBuf::Imported);
    }

    #[test]
    fn test_native_roundtrip() {
        let original = ShaderPathBuf::Native(PathBuf::from("/path/to/shader.wgsl"));
        let dynamic = original.to_dynamic();
        let roundtrip = ShaderPathBuf::from_dynamic(&dynamic, FromDynamicOptions::default()).unwrap();
        assert_eq!(original, roundtrip);
    }
}
