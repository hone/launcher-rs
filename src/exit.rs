//! Exit code definitions for the CNB Launcher.
//!
//! This module defines the [`ExitCode`] enum representing standard exit codes returned by the launcher.
//! These status codes conform to the Cloud Native Buildpacks (CNB) lifecycle specification contract.

/// Standard exit codes returned by the Cloud Native Buildpacks (CNB) launcher.
/// These exit codes are part of the launcher's public CLI contract with the platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum ExitCode {
    /// Generic or OS-level failure, such as file read errors or command execution failures.
    Failed = 1,
    /// The requested Platform API version is unsupported or empty.
    PlatformApiIncompatible = 11,
    /// One or more buildpack API versions are incompatible with the lifecycle.
    BuildpackApiIncompatible = 12,
    /// The targeted process failed to launch or execution failed.
    LaunchError = 82,
}

impl ExitCode {
    /// Casts the strongly-typed `ExitCode` enum to its primitive standard `i32` integer value.
    pub fn as_i32(self) -> i32 {
        self as i32
    }
}
