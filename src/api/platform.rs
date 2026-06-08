//! Platform API validation and compatibility checks.
//!
//! This module defines the Platform API versions supported by this launcher implementation,
//! provides utilities for checking version support and deprecation status, and exports
//! the [`verify_platform_api`] function to validate the `CNB_PLATFORM_API` environment variable.

use super::Version;
use std::str::FromStr;

/// The list of Platform API versions supported by this lifecycle launcher.
pub const SUPPORTED_PLATFORM_APIS: &[Version] = &[
    Version::new(0, 7),
    Version::new(0, 8),
    Version::new(0, 9),
    Version::new(0, 10),
    Version::new(0, 11),
    Version::new(0, 12),
    Version::new(0, 13),
    Version::new(0, 14),
    Version::new(0, 15),
];

/// The list of supported but deprecated Platform API versions.
/// Deprecated versions will issue a warning upon verification but are still allowed to execute.
pub const DEPRECATED_PLATFORM_APIS: &[Version] = &[];

/// Errors that can occur during Platform API verification.
#[derive(Debug, PartialEq, Eq)]
pub enum PlatformApiError {
    /// The requested Platform API version string was empty.
    Empty,
    /// Failed to parse the Platform API version string.
    Invalid(String),
    /// The parsed Platform API version is not supported by this lifecycle launcher.
    Incompatible(String),
}

impl std::fmt::Display for PlatformApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlatformApiError::Empty => write!(f, "Platform API version is empty"),
            PlatformApiError::Invalid(v) => write!(f, "parse platform API '{}'", v),
            PlatformApiError::Incompatible(v) => write!(
                f,
                "platform API version '{}' is incompatible with the lifecycle",
                v
            ),
        }
    }
}

/// Verifies whether the requested Platform API version string is supported.
///
/// # Arguments
///
/// * `requested_str` - The raw Platform API version string (typically from the `CNB_PLATFORM_API` environment variable).
///
/// # Errors
///
/// Returns [`PlatformApiError`] if the version string is empty, cannot be parsed,
/// or is incompatible with the supported API versions.
pub fn verify_platform_api(requested_str: &str) -> Result<Version, PlatformApiError> {
    let clean = requested_str.trim();
    if clean.is_empty() {
        return Err(PlatformApiError::Empty);
    }

    let requested =
        Version::from_str(clean).map_err(|_| PlatformApiError::Invalid(clean.to_string()))?;

    if super::is_supported(&requested, SUPPORTED_PLATFORM_APIS) {
        if super::is_deprecated(&requested, DEPRECATED_PLATFORM_APIS) {
            // Note: We can implement deprecation warnings if required
            eprintln!("Platform requested deprecated API '{}'", clean);
        }
        Ok(requested)
    } else {
        Err(PlatformApiError::Incompatible(clean.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_platform_api_verification() {
        assert!(verify_platform_api("0.15").is_ok());
        assert!(verify_platform_api("0.7").is_ok());
        assert_eq!(
            verify_platform_api("0.6"),
            Err(PlatformApiError::Incompatible("0.6".to_string()))
        );
        assert_eq!(
            verify_platform_api("bad-api"),
            Err(PlatformApiError::Invalid("bad-api".to_string()))
        );
        assert_eq!(verify_platform_api(""), Err(PlatformApiError::Empty));
    }
}
