//! Compilation and execution of CNB `exec.d` binaries.
//!
//! Before the application process is launched, buildpacks can run initialization binaries placed in
//! `exec.d/` directories. These programs output TOML key-value pairs representing environment variables
//! that must be set in the final process.
//!
//! Under Unix, the communication is established using an OS pipe mapped to File Descriptor 3 in the child.
//! Under Windows, the communication is established using an inherited Win32 pipe handle specified by
//! the `CNB_EXEC_D_HANDLE` environment variable.

use crate::env::LaunchEnv;
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::process::Command;

#[cfg(unix)]
use std::os::unix::io::AsRawFd;
#[cfg(unix)]
use std::os::unix::process::CommandExt;

/// Errors that can occur during execution of `exec.d` helper binaries.
#[derive(Debug)]
pub enum ExecDError {
    /// Failed to create the OS pipe for process communication.
    CreatePipe(String),
    /// Failed to spawn the `exec.d` child process.
    Spawn {
        /// The path of the binary that failed to spawn.
        path: String,
        /// The underlying I/O error.
        error: std::io::Error,
    },
    /// Failed to read output from the communication pipe.
    ReadOutput {
        /// The underlying I/O error.
        error: std::io::Error,
        /// Description of the channel (e.g., `"FD 3"`, `"handle"`).
        channel: &'static str,
    },
    /// Failed to wait for the child process to exit.
    Wait {
        /// The path of the binary.
        path: String,
        /// The underlying I/O error.
        error: std::io::Error,
    },
    /// The `exec.d` binary exited with a non-zero status.
    ExecutionFailed {
        /// The path of the binary.
        path: String,
        /// The exit status.
        status: std::process::ExitStatus,
    },
    /// Failed to parse the TOML map returned by the binary.
    Decode {
        /// The path of the binary.
        path: String,
        /// The TOML deserialization error.
        error: Box<toml::de::Error>,
        /// The raw output string that failed parsing.
        output: String,
    },
}

impl std::fmt::Display for ExecDError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExecDError::CreatePipe(msg) => write!(f, "{}", msg),
            ExecDError::Spawn { path, error } => {
                write!(f, "Failed to spawn exec.d binary '{}': {}", path, error)
            }
            ExecDError::ReadOutput { error, channel } => {
                write!(
                    f,
                    "Failed to read {} output from exec.d: {}",
                    channel, error
                )
            }
            ExecDError::Wait { path: _, error } => {
                write!(f, "Failed to wait for exec.d child process: {}", error)
            }
            ExecDError::ExecutionFailed { path, status } => {
                write!(f, "exec.d binary '{}' failed with status: {}", path, status)
            }
            ExecDError::Decode {
                path,
                error,
                output,
            } => {
                write!(
                    f,
                    "Failed to decode TOML output from exec.d binary '{}': {}\nOutput: '{}'",
                    path, error, output
                )
            }
        }
    }
}

impl std::error::Error for ExecDError {}

/// Executes the executable at the given path, capturing environment variables written to File Descriptor 3.
/// The executable should implement the exec.d interface, writing TOML key-value pairs to FD 3.
#[cfg(unix)]
pub fn run_exec_d<P: AsRef<Path>>(
    path: P,
    env: &LaunchEnv,
) -> Result<HashMap<String, String>, ExecDError> {
    let path = path.as_ref();
    let (reader_fd, writer_fd) = rustix::pipe::pipe()
        .map_err(|e| ExecDError::CreatePipe(format!("Failed to create OS pipe: {}", e)))?;

    let reader: File = reader_fd.into();
    let writer: File = writer_fd.into();

    let writer_fd = writer.as_raw_fd();

    let mut cmd = Command::new(path);
    cmd.envs(env.vars());

    unsafe {
        cmd.pre_exec(move || {
            // Duplicate the write end of the pipe to File Descriptor 3 in the child process
            if libc::dup2(writer_fd, 3) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let mut child = cmd.spawn().map_err(|e| ExecDError::Spawn {
        path: path.to_string_lossy().into_owned(),
        error: e,
    })?;

    // CRITICAL: Close our copy of the writer in the parent so the reader will receive EOF
    // once the child process closes its copy (e.g. upon exiting or explicitly closing FD 3).
    drop(writer);

    let mut toml_output = String::new();
    let mut r = reader;
    r.read_to_string(&mut toml_output)
        .map_err(|e| ExecDError::ReadOutput {
            error: e,
            channel: "FD 3",
        })?;

    let status = child.wait().map_err(|e| ExecDError::Wait {
        path: path.to_string_lossy().into_owned(),
        error: e,
    })?;

    if !status.success() {
        return Err(ExecDError::ExecutionFailed {
            path: path.to_string_lossy().into_owned(),
            status,
        });
    }

    if toml_output.trim().is_empty() {
        return Ok(HashMap::new());
    }

    let env_vars: HashMap<String, String> =
        toml::from_str(&toml_output).map_err(|e| ExecDError::Decode {
            path: path.to_string_lossy().into_owned(),
            error: Box::new(e),
            output: toml_output,
        })?;

    Ok(env_vars)
}

#[cfg(windows)]
#[allow(non_snake_case)]
/// Internal Windows API declarations for pipe creation and process communication.
mod win32 {
    use std::ffi::c_void;

    pub type HANDLE = *mut c_void;
    pub type BOOL = i32;
    pub type DWORD = u32;

    pub const HANDLE_FLAG_INHERIT: DWORD = 0x00000001;

    #[repr(C)]
    pub struct SECURITY_ATTRIBUTES {
        pub nLength: DWORD,
        pub lpSecurityDescriptor: *mut c_void,
        pub bInheritHandle: BOOL,
    }

    unsafe extern "system" {
        pub fn CreatePipe(
            hReadPipe: *mut HANDLE,
            hWritePipe: *mut HANDLE,
            lpPipeAttributes: *mut SECURITY_ATTRIBUTES,
            nSize: DWORD,
        ) -> BOOL;
        pub fn SetHandleInformation(hObject: HANDLE, dwMask: DWORD, dwFlags: DWORD) -> BOOL;
    }
}

/// Executes the executable at the given path on Windows, capturing environment variables
/// written to the inherited pipe handle specified by CNB_EXEC_D_HANDLE.
#[cfg(windows)]
pub fn run_exec_d<P: AsRef<Path>>(
    path: P,
    env: &LaunchEnv,
) -> Result<HashMap<String, String>, ExecDError> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use std::process::Stdio;

    let path = path.as_ref();
    let mut read_handle: win32::HANDLE = std::ptr::null_mut();
    let mut write_handle: win32::HANDLE = std::ptr::null_mut();

    let mut sa = win32::SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<win32::SECURITY_ATTRIBUTES>() as win32::DWORD,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: 1, // TRUE
    };

    let res = unsafe { win32::CreatePipe(&mut read_handle, &mut write_handle, &mut sa, 0) };

    if res == 0 {
        return Err(ExecDError::CreatePipe(
            "Failed to create Win32 pipe".to_string(),
        ));
    }

    // Disable read handle inheritance so only the write handle is inherited by the child
    if unsafe { win32::SetHandleInformation(read_handle, win32::HANDLE_FLAG_INHERIT, 0) } == 0 {
        return Err(ExecDError::CreatePipe(
            "Failed to configure pipe handle inheritance".to_string(),
        ));
    }

    // Convert raw handles into Rust File objects
    let reader: File = unsafe { File::from_raw_handle(read_handle) };
    let writer: File = unsafe { File::from_raw_handle(write_handle) };

    let write_handle_val = writer.as_raw_handle();
    let handle_hex = format!("{:#x}", write_handle_val as usize);

    let mut cmd = Command::new(path);
    cmd.stdout(Stdio::inherit());
    cmd.stderr(Stdio::inherit());

    // Inject CNB_EXEC_D_HANDLE variable
    let mut child_env = env.vars().clone();
    child_env.insert("CNB_EXEC_D_HANDLE".to_string(), handle_hex);
    cmd.envs(&child_env);

    let mut child = cmd.spawn().map_err(|e| ExecDError::Spawn {
        path: path.to_string_lossy().into_owned(),
        error: e,
    })?;

    // Close parent's copy of writer
    drop(writer);

    let mut toml_output = String::new();
    let mut r = reader;
    r.read_to_string(&mut toml_output)
        .map_err(|e| ExecDError::ReadOutput {
            error: e,
            channel: "handle",
        })?;

    let status = child.wait().map_err(|e| ExecDError::Wait {
        path: path.to_string_lossy().into_owned(),
        error: e,
    })?;

    if !status.success() {
        return Err(ExecDError::ExecutionFailed {
            path: path.to_string_lossy().into_owned(),
            status,
        });
    }

    if toml_output.trim().is_empty() {
        return Ok(HashMap::new());
    }

    let env_vars: HashMap<String, String> =
        toml::from_str(&toml_output).map_err(|e| ExecDError::Decode {
            path: path.to_string_lossy().into_owned(),
            error: Box::new(e),
            output: toml_output,
        })?;

    Ok(env_vars)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    #[test]
    fn test_exec_d_runner() {
        let dir = tempdir().unwrap();
        let script_path = dir.path().join("mock_exec_d.sh");

        // Write a simple bash script that outputs to FD 3
        let script_content = r#"#!/bin/bash
echo 'MY_NEW_VAR = "injected_value"' >&3
"#;
        fs::write(&script_path, script_content).unwrap();

        // Make the script executable
        let mut perms = fs::metadata(&script_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script_path, perms).unwrap();

        let env = LaunchEnv::new(&[], "", "");
        let res = run_exec_d(&script_path, &env);

        assert!(res.is_ok(), "Failed to run exec.d: {:?}", res.err());
        let vars = res.unwrap();
        assert_eq!(vars.get("MY_NEW_VAR").unwrap(), "injected_value");
    }
}
