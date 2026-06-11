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

#[cfg(test)]
mod ported_rust_tests {
    use super::*;
    use crate::exit::ExitCode;
    use crate::LaunchError;

    // Ported from launcher-rust src/api.rs `verify_buildpack_api_supported`.
    // "0.10" is in SUPPORTED_BUILDPACK_APIS -> Ok(Version { major: 0, minor: 10 }).
    #[test]
    fn verify_buildpack_api_supported() {
        let v = verify_buildpack_api("foo", "0.10").unwrap();
        assert_eq!(v, Version { major: 0, minor: 10 });
    }

    // Ported from launcher-rust src/api.rs `verify_buildpack_api_unsupported`.
    // GAP: launcher-rust pins the Go-faithful Display
    //   "failed to set API for buildpack 'foo': buildpack API version '0.5' is
    //    incompatible with the lifecycle"
    // (action prefix from cmd/apis.go + err with NO trailing buildpack id).
    // launcher-rs surfaces this incompatibility via LaunchError::SetBuildpackApi
    // wrapping BuildpackApiError::Incompatible, whose Display is
    //   "failed to set API for buildpack 'foo': buildpack API version '0.5' is
    //    incompatible with the lifecycle for buildpack 'foo'"
    // (BuildpackApiError::Incompatible appends "for buildpack 'foo'", src/api/buildpack.rs:37-39).
    // The assert_eq! against the Go-faithful string FAILS, exposing the message-format divergence.
    #[test]
    fn verify_buildpack_api_unsupported() {
        let inner = verify_buildpack_api("foo", "0.5").unwrap_err();
        assert!(matches!(inner, BuildpackApiError::Incompatible { .. }));
        let err = LaunchError::SetBuildpackApi {
            bp_id: "foo".to_string(),
            error: inner,
        };
        assert_eq!(err.code(), ExitCode::BuildpackApiIncompatible);
        assert_eq!(
            err.to_string(),
            "failed to set API for buildpack 'foo': buildpack API version '0.5' is incompatible with the lifecycle"
        );
    }

    // Ported from launcher-rust src/api.rs `verify_buildpack_api_unparseable`.
    // GAP: launcher-rust pins the Go-faithful Display
    //   "failed to parse buildpack API 'abc' for buildpack 'foo'"
    // (mirrors Go cmd/apis.go:43 "parse buildpack API ... for buildpack ...").
    // launcher-rs surfaces this via LaunchError::SetBuildpackApi wrapping
    // BuildpackApiError::Parse, whose Display is
    //   "failed to set API for buildpack 'foo': Parse buildpack API 'abc' for
    //    buildpack 'foo': Parsing Major 'abc'"
    // (capitalized "Parse", no "failed to parse" form, trailing inner ": Parsing Major 'abc'",
    //  src/api/buildpack.rs:27). The assert_eq! against the Go-faithful string FAILS.
    #[test]
    fn verify_buildpack_api_unparseable() {
        let inner = verify_buildpack_api("foo", "abc").unwrap_err();
        assert!(matches!(inner, BuildpackApiError::Parse { .. }));
        let err = LaunchError::SetBuildpackApi {
            bp_id: "foo".to_string(),
            error: inner,
        };
        assert_eq!(err.code(), ExitCode::BuildpackApiIncompatible);
        assert_eq!(
            err.to_string(),
            "failed to parse buildpack API 'abc' for buildpack 'foo'"
        );
    }

    // Ported from launcher-rust src/api.rs deprecation handling.
    // DEPRECATED_BUILDPACK_APIS is empty, so a supported version is never flagged
    // deprecated and is returned Ok regardless of CNB_DEPRECATION_MODE.
    #[test]
    fn verify_buildpack_api_deprecated_allowed() {
        let v = verify_buildpack_api("foo", "0.12").unwrap();
        assert_eq!(v, Version { major: 0, minor: 12 });
        assert!(!super::super::is_deprecated(&v, DEPRECATED_BUILDPACK_APIS));
    }

    // An empty buildpack API string is unparseable.
    // launcher-rs trims then Version::from_str("") -> Err -> BuildpackApiError::Parse.
    #[test]
    fn verify_buildpack_api_empty() {
        let err = verify_buildpack_api("foo", "").unwrap_err();
        assert!(matches!(err, BuildpackApiError::Parse { .. }));
    }

    // Ported from launcher-rust src/exit.rs `incompat_buildpack_api_code_is_12`.
    // Buildpack-API incompatibility reports exit code 12 via both mapping paths.
    #[test]
    fn verify_buildpack_api_code_12() {
        // (a) raw exit-code value.
        assert_eq!(ExitCode::BuildpackApiIncompatible.as_i32(), 12);

        let incompat = BuildpackApiError::Incompatible {
            bp_id: "foo".to_string(),
            version: "0.5".to_string(),
        };

        // (b) the set-API path.
        let set_err = LaunchError::SetBuildpackApi {
            bp_id: "foo".to_string(),
            error: incompat.clone(),
        };
        assert_eq!(set_err.code(), ExitCode::BuildpackApiIncompatible);

        // (c) the process-selection path.
        let sel_err = LaunchError::ProcessSelection(Box::new(
            crate::launch::ProcessSelectionError::BuildpackApi(incompat),
        ));
        assert_eq!(sel_err.code(), ExitCode::BuildpackApiIncompatible);
    }
}

