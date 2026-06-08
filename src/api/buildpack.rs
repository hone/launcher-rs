//! Buildpack API validation and compatibility checks.
//!
//! This module defines the Buildpack API versions supported by this launcher implementation,
//! provides utilities to check support and deprecation status, and exports the [`verify_buildpack_api`]
//! function to validate buildpack-contributed API versions declared in the metadata.

use super::Version;
use std::str::FromStr;

/// The list of Buildpack API versions supported by this lifecycle launcher.
pub const SUPPORTED_BUILDPACK_APIS: &[&str] = &["0.7", "0.8", "0.9", "0.10", "0.11", "0.12"];

/// The list of supported but deprecated Buildpack API versions.
pub const DEPRECATED_BUILDPACK_APIS: &[&str] = &[];

/// Checks if the requested Buildpack API [`Version`] is supported by the launcher.
///
/// Under the CNB specification, a buildpack API version is supported if any version in
/// [`SUPPORTED_BUILDPACK_APIS`] is a superset of (compatible with) the requested version.
pub fn is_supported(requested: &Version) -> bool {
    SUPPORTED_BUILDPACK_APIS.iter().any(|&sup| {
        if let Ok(sup_ver) = Version::from_str(sup) {
            sup_ver.is_superset_of(requested)
        } else {
            false
        }
    })
}

/// Checks if the requested Buildpack API [`Version`] is deprecated.
pub fn is_deprecated(requested: &Version) -> bool {
    DEPRECATED_BUILDPACK_APIS.iter().any(|&dep| {
        if let Ok(dep_ver) = Version::from_str(dep) {
            dep_ver.is_superset_of(requested)
        } else {
            false
        }
    })
}

/// Errors that can occur during Buildpack API verification.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum BuildpackApiError {
    /// Failed to parse the Buildpack API version string.
    Parse {
        /// The identifier of the buildpack.
        bp_id: String,
        /// The raw version string that failed parsing.
        version: String,
        /// The inner parsing error description.
        error: String,
    },
    /// The Buildpack API version is not supported by this lifecycle launcher.
    Incompatible {
        /// The identifier of the buildpack.
        bp_id: String,
        /// The incompatible version string.
        version: String,
    },
}

impl std::fmt::Display for BuildpackApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildpackApiError::Parse {
                bp_id,
                version,
                error,
            } => {
                write!(
                    f,
                    "Parse buildpack API '{}' for buildpack '{}': {}",
                    version, bp_id, error
                )
            }
            BuildpackApiError::Incompatible { bp_id, version } => {
                write!(
                    f,
                    "buildpack API version '{}' is incompatible with the lifecycle for buildpack '{}'",
                    version, bp_id
                )
            }
        }
    }
}

impl std::error::Error for BuildpackApiError {}

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

    if is_supported(&requested) {
        if is_deprecated(&requested) {
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
