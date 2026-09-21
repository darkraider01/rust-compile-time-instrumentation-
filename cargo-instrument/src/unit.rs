use serde::{Deserialize, Serialize};
use std::fmt;

/// Unique identifier for a compilation unit within a Cargo build session.
///
/// In Cargo, `--crate-name` is not unique across compilation units:
/// - Cargo compiles multiple versions of the same package (e.g. `syn 1.0` and `syn 2.0`)
///   with identical `--crate-name syn`.
/// - Cargo compiles both the library rlib and the integration test harness of the same package
///   with identical `--crate-name <crate>`.
///
/// Cargo passes each compilation unit a unique metadata hash via `-C metadata=<hash>`.
/// `UnitId` pairs the crate name with this disambiguating metadata hash.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UnitId {
    pub crate_name: String,
    pub metadata_hash: Option<String>,
}

impl UnitId {
    /// Create a new `UnitId` with a crate name and optional metadata hash.
    pub fn new(crate_name: impl Into<String>, metadata_hash: Option<String>) -> Self {
        Self {
            crate_name: crate_name.into(),
            metadata_hash,
        }
    }

    /// Construct a UnitId with only a crate name (deterministic fallback when metadata is unavailable).
    pub fn from_crate_name(crate_name: impl Into<String>) -> Self {
        Self {
            crate_name: crate_name.into(),
            metadata_hash: None,
        }
    }

    /// The isolated directory name for this unit's mirror under `instrumented_sources/`.
    ///
    /// If `metadata_hash` is present, returns `{crate_name}-{metadata_hash}`.
    /// If absent, falls back deterministically to `{crate_name}`.
    pub fn dir_name(&self) -> String {
        match &self.metadata_hash {
            Some(hash) => format!("{}-{}", self.crate_name, hash),
            None => self.crate_name.clone(),
        }
    }

    /// Name of the marker proving that an exact Cargo artifact was compiled
    /// from a mirrored, transformed source tree. Cargo places `extra_filename`
    /// in the artifact path, so it is the identity shared by the wrapper and
    /// Cargo's `compiler-artifact` message. A crate name alone is insufficient.
    pub fn instrumentation_stamp_name(crate_name: &str, extra_filename: &str) -> Option<String> {
        let artifact_hash = extra_filename.strip_prefix('-')?;
        if artifact_hash.is_empty()
            || !artifact_hash
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return None;
        }
        Some(format!(
            ".cargo-instrument-transformed-{}-{}.stamp",
            crate_name.replace('-', "_"),
            artifact_hash
        ))
    }
}

impl fmt::Display for UnitId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.dir_name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unit_id_with_metadata() {
        let id = UnitId::new("foo", Some("1234567890abcdef".to_string()));
        assert_eq!(id.dir_name(), "foo-1234567890abcdef");
        assert_eq!(id.to_string(), "foo-1234567890abcdef");
    }

    #[test]
    fn test_unit_id_fallback_without_metadata() {
        let id = UnitId::from_crate_name("bar");
        assert_eq!(id.dir_name(), "bar");
        assert_eq!(id.to_string(), "bar");
        assert_eq!(UnitId::instrumentation_stamp_name("bar", "hash"), None);
    }

    #[test]
    fn test_instrumentation_stamp_name_is_unit_specific() {
        assert_eq!(
            UnitId::instrumentation_stamp_name("foo_bar", "-first").as_deref(),
            Some(".cargo-instrument-transformed-foo_bar-first.stamp")
        );
        assert_ne!(
            UnitId::instrumentation_stamp_name("foo_bar", "-first"),
            UnitId::instrumentation_stamp_name("foo_bar", "-second")
        );
    }
}
