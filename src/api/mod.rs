//! API version validation and handling for the Cloud Native Buildpacks (CNB) Launcher.
//!
//! This module provides the [`Version`] struct to parse and compare SemVer-like version
//! strings (e.g., "0.7", "1.12") representing Platform and Buildpack API versions.
//! It also exposes the [`platform`] and [`buildpack`] submodules to enforce compatibility
//! rules according to CNB specification requirements.

use std::fmt;
use std::str::FromStr;

/// Rules and validation logic for Cloud Native Buildpack API versions.
pub mod buildpack;
/// Rules and validation logic for Cloud Native Platform API versions.
pub mod platform;

/// Represents a SemVer-like Platform or Buildpack API version, supporting major and minor keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    /// The major version number.
    pub major: u64,
    /// The minor version number.
    pub minor: u64,
}

impl Version {
    /// Creates a new `Version` instance with the specified major and minor version numbers.
    pub const fn new(major: u64, minor: u64) -> Self {
        Version { major, minor }
    }

    /// Determines if this version is a superset of (backwards-compatible with) another `Version`.
    ///
    /// According to the Cloud Native Buildpacks specification:
    /// - For major version 0, they must be exactly equal (strict backwards-incompatible 0.x phase).
    /// - For major version >= 1, they must have the same major version and this minor >= other minor.
    pub fn is_superset_of(&self, other: &Version) -> bool {
        if self.major == 0 {
            self == other
        } else {
            self.major == other.major && self.minor >= other.minor
        }
    }

    /// Checks if this version is strictly less than another version string.
    /// Returns `false` if the comparison string fails to parse.
    pub fn less_than(&self, other: &str) -> bool {
        if let Ok(other_ver) = Version::from_str(other) {
            self < &other_ver
        } else {
            false
        }
    }

    /// Checks if this version is at least (greater than or equal to) another version string.
    /// Returns `false` if the comparison string fails to parse.
    pub fn at_least(&self, other: &str) -> bool {
        if let Ok(other_ver) = Version::from_str(other) {
            self >= &other_ver
        } else {
            false
        }
    }
}

impl FromStr for Version {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let clean = s.trim_start_matches('v').trim();
        let parts: Vec<&str> = clean.split('.').collect();
        if parts.is_empty() || parts[0].is_empty() {
            return Err(format!("Could not parse '{}' as version", s));
        }

        let major = parts[0]
            .parse::<u64>()
            .map_err(|_| format!("Parsing Major '{}'", parts[0]))?;

        let minor = if parts.len() > 1 && !parts[1].is_empty() {
            parts[1]
                .parse::<u64>()
                .map_err(|_| format!("Parsing Minor '{}'", parts[1]))?
        } else {
            0
        };

        Ok(Version { major, minor })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// Checks if a version is compatible with any of the API versions in the list.
fn is_supported(requested: &Version, list: &[Version]) -> bool {
    list.iter().any(|v| v.is_superset_of(requested))
}

/// Checks if a version is deprecated by any of the API versions in the list.
fn is_deprecated(requested: &Version, list: &[Version]) -> bool {
    list.iter().any(|v| v.is_superset_of(requested))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_parsing() {
        assert_eq!(
            Version::from_str("0.7").unwrap(),
            Version { major: 0, minor: 7 }
        );
        assert_eq!(
            Version::from_str("v0.7").unwrap(),
            Version { major: 0, minor: 7 }
        );
        assert_eq!(
            Version::from_str("1.12").unwrap(),
            Version {
                major: 1,
                minor: 12
            }
        );
        assert_eq!(
            Version::from_str("2").unwrap(),
            Version { major: 2, minor: 0 }
        );
        assert!(Version::from_str("").is_err());
        assert!(Version::from_str("abc").is_err());
    }

    #[test]
    fn test_version_display() {
        assert_eq!(
            Version {
                major: 0,
                minor: 15
            }
            .to_string(),
            "0.15"
        );
        assert_eq!(Version { major: 1, minor: 0 }.to_string(), "1.0");
    }

    #[test]
    fn test_is_superset_of() {
        // 0.x versions must be exactly equal
        let v0_7 = Version { major: 0, minor: 7 };
        let v0_8 = Version { major: 0, minor: 8 };
        assert!(v0_7.is_superset_of(&v0_7));
        assert!(!v0_7.is_superset_of(&v0_8));

        // >= 1.x versions are backwards compatible within the same major
        let v1_5 = Version { major: 1, minor: 5 };
        let v1_2 = Version { major: 1, minor: 2 };
        let v1_8 = Version { major: 1, minor: 8 };
        let v2_0 = Version { major: 2, minor: 0 };
        assert!(v1_5.is_superset_of(&v1_2));
        assert!(v1_5.is_superset_of(&v1_5));
        assert!(!v1_5.is_superset_of(&v1_8));
        assert!(!v1_5.is_superset_of(&v2_0));
    }

    #[test]
    fn test_less_than_and_at_least() {
        let v0_10 = Version {
            major: 0,
            minor: 10,
        };
        assert!(v0_10.less_than("0.11"));
        assert!(!v0_10.less_than("0.9"));
        assert!(v0_10.at_least("0.10"));
        assert!(v0_10.at_least("0.4"));
    }
}
