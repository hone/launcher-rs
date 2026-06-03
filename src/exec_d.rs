use crate::env::LaunchEnv;
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::process::Command;

#[cfg(unix)]
use std::os::unix::io::AsRawFd;
#[cfg(unix)]
use std::os::unix::io::FromRawFd;
#[cfg(unix)]
use std::os::unix::io::IntoRawFd;
#[cfg(unix)]
use std::os::unix::process::CommandExt;

#[derive(Debug)]
pub enum ExecDError {
    CreatePipe(String),
    Spawn {
        path: String,
        error: std::io::Error,
    },
    ReadOutput {
        error: std::io::Error,
        channel: &'static str,
    },
    Wait {
        path: String,
        error: std::io::Error,
    },
    ExecutionFailed {
        path: String,
        status: std::process::ExitStatus,
    },
    Decode {
        path: String,
        error: Box<toml::de::Error>,
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
pub fn run_exec_d(path: &str, env: &LaunchEnv) -> Result<HashMap<String, String>, ExecDError> {
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
            let borrowed_writer = std::os::unix::io::BorrowedFd::borrow_raw(writer_fd);

            // We construct an OwnedFd for FD 3, which dup2 requires as the target.
            // This is unsafe because we are claiming ownership of FD 3, which is fine
            // since we are in the child process and about to either overwrite it or fail.
            let mut target = std::os::unix::io::OwnedFd::from_raw_fd(3);

            if let Err(e) = rustix::io::dup2(borrowed_writer, &mut target) {
                // Leak target so it doesn't try to close an invalid FD on error
                let _ = target.into_raw_fd();
                return Err(e.into());
            }

            // Leak target so FD 3 stays open for the exec'd process!
            let _ = target.into_raw_fd();
            Ok(())
        });
    }

    let mut child = cmd.spawn().map_err(|e| ExecDError::Spawn {
        path: path.to_string(),
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
        path: path.to_string(),
        error: e,
    })?;

    if !status.success() {
        return Err(ExecDError::ExecutionFailed {
            path: path.to_string(),
            status,
        });
    }

    if toml_output.trim().is_empty() {
        return Ok(HashMap::new());
    }

    let env_vars: HashMap<String, String> = toml::from_str(&toml_output).map_err(|e| {
        ExecDError::Decode {
            path: path.to_string(),
            error: Box::new(e),
            output: toml_output,
        }
    })?;

    Ok(env_vars)
}

#[cfg(windows)]
mod win32 {
    use std::ffi::c_void;

    pub type HANDLE = *mut c_void;
    pub type BOOL = i32;
    pub type DWORD = u32;

    #[repr(C)]
    pub struct SECURITY_ATTRIBUTES {
        pub nLength: DWORD,
        pub lpSecurityDescriptor: *mut c_void,
        pub bInheritHandle: BOOL,
    }

    extern "system" {
        pub fn CreatePipe(
            hReadPipe: *mut HANDLE,
            hWritePipe: *mut HANDLE,
            lpPipeAttributes: *mut SECURITY_ATTRIBUTES,
            nSize: DWORD,
        ) -> BOOL;
    }
}

/// Executes the executable at the given path on Windows, capturing environment variables
/// written to the inherited pipe handle specified by CNB_EXEC_D_HANDLE.
#[cfg(windows)]
pub fn run_exec_d(path: &str, env: &LaunchEnv) -> Result<HashMap<String, String>, ExecDError> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use std::process::Stdio;

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
        path: path.to_string(),
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
        path: path.to_string(),
        error: e,
    })?;

    if !status.success() {
        return Err(ExecDError::ExecutionFailed {
            path: path.to_string(),
            status,
        });
    }

    if toml_output.trim().is_empty() {
        return Ok(HashMap::new());
    }

    let env_vars: HashMap<String, String> =
        toml::from_str(&toml_output).map_err(|e| ExecDError::Decode {
            path: path.to_string(),
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
        let res = run_exec_d(&script_path.to_string_lossy(), &env);

        assert!(res.is_ok(), "Failed to run exec.d: {:?}", res.err());
        let vars = res.unwrap();
        assert_eq!(vars.get("MY_NEW_VAR").unwrap(), "injected_value");
    }
}
