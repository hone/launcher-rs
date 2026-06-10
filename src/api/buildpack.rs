//! Buildpack API validation and compatibility checks.
//!
//! This module defines the Buildpack API versions supported by this launcher implementation,
//! provides utilities to check support and deprecation status, and exports the [`verify_buildpack_api`]
//! function to validate buildpack-contributed API versions declared in the metadata.

use super::Version;
use std::str::FromStr;

/// The list of Buildpack API versions supported by this lifecycle launcher.
pub const SUPPORTED_BUILDPACK_APIS: &[Version] = &[
    Version::new(0, 7),
    Version::new(0, 8),
    Version::new(0, 9),
    Version::new(0, 10),
    Version::new(0, 11),
    Version::new(0, 12),
];

/// The list of supported but deprecated Buildpack API versions.
pub const DEPRECATED_BUILDPACK_APIS: &[Version] = &[];

/// Errors that can occur during Buildpack API verification.
#[derive(Debug, PartialEq, Eq, Clone, thiserror::Error)]
pub enum BuildpackApiError {
    /// Failed to parse the Buildpack API version string.
    #[error("Parse buildpack API '{version}' for buildpack '{bp_id}': {error}")]
    Parse {
        /// The identifier of the buildpack.
        bp_id: String,
        /// The raw version string that failed parsing.
        version: String,
        /// The inner parsing error description.
        error: String,
    },
    /// The Buildpack API version is not supported by this lifecycle launcher.
    #[error(
        "buildpack API version '{version}' is incompatible with the lifecycle for buildpack '{bp_id}'"
    )]
    Incompatible {
        /// The identifier of the buildpack.
        bp_id: String,
        /// The incompatible version string.
        version: String,
    },
}

/// Verifies whether the requested Buildpack API version string is supported.
///
/// # Arguments
///
/// * `bp_id` - The identifier of the buildpack.
/// * `requested_str` - The raw buildpack API version string declared in the buildpack metadata.
///
/// # Errors
///
/// Returns [`BuildpackApiError`] if the version string cannot be parsed or is incompatible.
pub fn verify_buildpack_api(
    bp_id: &str,
    requested_str: &str,
) -> Result<Version, BuildpackApiError> {
    let clean = requested_str.trim();
    let requested = Version::from_str(clean).map_err(|e| BuildpackApiError::Parse {
        bp_id: bp_id.to_string(),
        version: clean.to_string(),
        error: e,
    })?;

    if super::is_supported(&requested, SUPPORTED_BUILDPACK_APIS) {
        if super::is_deprecated(&requested, DEPRECATED_BUILDPACK_APIS) {
            eprintln!("Buildpack '{}' requested deprecated API '{}'", bp_id, clean);
        }
        Ok(requested)
    } else {
        Err(BuildpackApiError::Incompatible {
            bp_id: bp_id.to_string(),
            version: clean.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_buildpack_api_verification() {
        assert!(verify_buildpack_api("my-bp", "0.12").is_ok());
        assert!(verify_buildpack_api("my-bp", "0.7").is_ok());
        assert!(verify_buildpack_api("my-bp", "0.6").is_err());
    }
}
