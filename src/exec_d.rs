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
#[derive(Debug, thiserror::Error)]
pub enum ExecDError {
    /// Failed to create the OS pipe for process communication.
    #[error("{0}")]
    CreatePipe(String),
    /// Failed to spawn the `exec.d` child process.
    #[error("Failed to spawn exec.d binary '{path}': {error}")]
    Spawn {
        /// The path of the binary that failed to spawn.
        path: String,
        /// The underlying I/O error.
        error: std::io::Error,
    },
    /// Failed to read output from the communication pipe.
    #[error("Failed to read {channel} output from exec.d: {error}")]
    ReadOutput {
        /// The underlying I/O error.
        error: std::io::Error,
        /// Description of the channel (e.g., `"FD 3"`, `"handle"`).
        channel: &'static str,
    },
    /// Failed to wait for the child process to exit.
    #[error("Failed to wait for exec.d child process: {error}")]
    Wait {
        /// The path of the binary.
        path: String,
        /// The underlying I/O error.
        error: std::io::Error,
    },
    /// The `exec.d` binary exited with a non-zero status.
    #[error("exec.d binary '{path}' failed with status: {status}")]
    ExecutionFailed {
        /// The path of the binary.
        path: String,
        /// The exit status.
        status: std::process::ExitStatus,
    },
    /// Failed to parse the TOML map returned by the binary.
    #[error(
        "Failed to decode TOML output from exec.d binary '{path}': {error}\nOutput: '{output}'"
    )]
    Decode {
        /// The path of the binary.
        path: String,
        /// The TOML deserialization error.
        error: Box<toml::de::Error>,
        /// The raw output string that failed parsing.
        output: String,
    },
}

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

        let env = LaunchEnv::new(std::iter::empty(), "", "");
        let res = run_exec_d(&script_path, &env);

        assert!(res.is_ok(), "Failed to run exec.d: {:?}", res.err());
        let vars = res.unwrap();
        assert_eq!(vars.get("MY_NEW_VAR").unwrap(), "injected_value");
    }
}

// ===========================================================================
// Ported from launcher-rust src/launch/exec_d.rs #[cfg(test)] mod tests.
// Appended as a self-contained module owning a unique name so it does not
// collide with the file's existing `mod tests`.
//
// launcher-rs's `run_exec_d` is only defined under #[cfg(unix)] (it returns
// the parsed map; the Windows variant has a different code path but the same
// signature). These behavioral tests spawn shell scripts via FD3 so they are
// unix-gated to match where `run_exec_d` + the shell helpers exist.
// ===========================================================================
#[cfg(all(test, unix))]
mod ported_rust_tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    fn write_script(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
        let p = dir.join(name);
        fs::write(&p, body).unwrap();
        let mut perm = fs::metadata(&p).unwrap().permissions();
        perm.set_mode(0o755);
        fs::set_permissions(&p, perm).unwrap();
        p
    }

    fn empty_env() -> LaunchEnv {
        // Construct a LaunchEnv with no inherited host vars, mirroring Go's
        // `env.List()` empty starting point.
        LaunchEnv::new(std::iter::empty(), "", "")
    }

    // Mirrors launcher-rust `sets_env_var_from_fd3_toml`.
    // Go-correct: FD3 `FOO = "bar"` yields FOO=bar. launcher-rs returns the
    // parsed map (it does not mutate env), so assert on the returned map.
    #[test]
    fn sets_env_var_from_fd3_toml() {
        let dir = tempdir().unwrap();
        let bin = write_script(
            dir.path(),
            "execd",
            "#!/bin/sh\nprintf 'FOO = \"bar\"\\n' >&3\n",
        );
        let env = empty_env();
        let vars = run_exec_d(&bin, &env).unwrap();
        assert_eq!(vars.get("FOO").map(String::as_str), Some("bar"));
    }

    // Mirrors launcher-rust `empty_fd3_output_is_ok`.
    // Go-correct: empty FD3 -> Ok, preset env preserved. launcher-rs returns
    // Ok(empty map) and never touches env, so a preset value survives.
    #[test]
    fn empty_fd3_output_is_ok() {
        let dir = tempdir().unwrap();
        let bin = write_script(dir.path(), "execd", "#!/bin/sh\n# write nothing to fd3\n");
        let mut env = empty_env();
        env.set("PRESET", "keep");
        let vars = run_exec_d(&bin, &env).unwrap();
        assert!(vars.is_empty(), "expected empty map, got {:?}", vars);
        assert_eq!(env.get("PRESET").map(String::as_str), Some("keep"));
    }

    // Mirrors launcher-rust `multiple_vars_all_set`.
    // Go-correct: both FOO and BAZ set.
    #[test]
    fn multiple_vars_all_set() {
        let dir = tempdir().unwrap();
        let bin = write_script(
            dir.path(),
            "execd",
            "#!/bin/sh\nprintf 'FOO = \"bar\"\\nBAZ = \"qux\"\\n' >&3\n",
        );
        let env = empty_env();
        let vars = run_exec_d(&bin, &env).unwrap();
        assert_eq!(vars.get("FOO").map(String::as_str), Some("bar"));
        assert_eq!(vars.get("BAZ").map(String::as_str), Some("qux"));
    }

    // GAP: launcher-rs's run_exec_d takes `&LaunchEnv` and RETURNS a
    // HashMap; it never mutates `env`. Go's ExecD calls env.Set(k, v) per
    // entry (lifecycle/launch/exec_d.go), and launcher-rust mirrors that by
    // mutating its `&mut Env`. Go-correct expectation: after the call, `env`
    // itself reflects FOO=bar. launcher-rs leaves `env` untouched, so this
    // assertion FAILS (divergence: returns-map vs mutates-env).
    #[test]
    fn port_run_exec_d_mutates_env_like_go() {
        let dir = tempdir().unwrap();
        let bin = write_script(
            dir.path(),
            "execd",
            "#!/bin/sh\nprintf 'FOO = \"bar\"\\n' >&3\n",
        );
        let env = empty_env();
        let _ = run_exec_d(&bin, &env).unwrap();
        // Go-faithful: env IS mutated. launcher-rs does not mutate -> FAILS.
        assert_eq!(env.get("FOO").map(String::as_str), Some("bar"));
    }

    // GAP: launcher-rust's non-zero-exit error message is
    // "failed to execute exec.d file at path '<path>': <code>". launcher-rs's
    // ExecDError::ExecutionFailed Display is
    // "exec.d binary '<path>' failed with status: <status>" (exec_d.rs:54).
    // The structural variant match always holds and the path is present, but
    // the Go-faithful message wording assertion FAILS (divergence: error
    // message format).
    #[test]
    fn port_nonzero_exit_returns_error_with_path() {
        let dir = tempdir().unwrap();
        let bin = write_script(dir.path(), "execd", "#!/bin/sh\nexit 7\n");
        let env = empty_env();
        let err = run_exec_d(&bin, &env).expect_err("expected error from non-zero exit");
        // Structural check (always holds against launcher-rs).
        assert!(
            matches!(err, ExecDError::ExecutionFailed { .. }),
            "expected ExecutionFailed, got {:?}",
            err
        );
        let msg = err.to_string();
        // Go-faithful wording -> FAILS against launcher-rs wording.
        assert!(
            msg.contains("failed to execute exec.d file at path"),
            "{}",
            msg
        );
        assert!(msg.contains(&bin.display().to_string()), "{}", msg);
    }

    // GAP: launcher-rust's decode error message is
    // "failed to decode output from exec.d file at path '<path>': <err>".
    // launcher-rs's ExecDError::Decode Display is
    // "Failed to decode TOML output from exec.d binary '<path>': <err>..."
    // (exec_d.rs:62-64). The structural Decode variant match holds, but the
    // Go-faithful message wording assertion FAILS (divergence: error message
    // format).
    #[test]
    fn port_invalid_toml_returns_decode_error() {
        let dir = tempdir().unwrap();
        let bin = write_script(
            dir.path(),
            "execd",
            "#!/bin/sh\nprintf 'this is not toml = =\\n' >&3\n",
        );
        let env = empty_env();
        let err = run_exec_d(&bin, &env).expect_err("expected decode error");
        // Structural check (always holds against launcher-rs).
        assert!(
            matches!(err, ExecDError::Decode { .. }),
            "expected Decode, got {:?}",
            err
        );
        let msg = err.to_string();
        // Go-faithful wording -> FAILS against launcher-rs wording.
        assert!(
            msg.contains("failed to decode output from exec.d file at path"),
            "{}",
            msg
        );
    }

    // GAP: Go sets `cmd.Env = env.List()` (full replacement: the child sees
    // ONLY env's vars), and launcher-rust mirrors this with `.env_clear()`.
    // launcher-rs uses `cmd.envs(env.vars())` with NO env_clear (exec_d.rs:92),
    // so the parent process environment LEAKS into the exec.d child.
    // Go-correct: a parent-only var must NOT be visible to the child, so the
    // script reports SEEN="no". launcher-rs leaks it -> SEEN="yes" and this
    // assertion FAILS (divergence: missing env_clear).
    #[test]
    fn port_child_env_is_cleared_like_go() {
        let marker = "PORT_EXECD_LEAK_MARKER";
        // SAFETY: single-threaded test setup; value read by the child only.
        unsafe { std::env::set_var(marker, "leaked") };

        let dir = tempdir().unwrap();
        // The script reports whether the marker is visible, via FD3 TOML.
        let body = format!(
            "#!/bin/sh\nif [ -n \"${{{m}}}\" ]; then printf 'SEEN = \"yes\"\\n' >&3; else printf 'SEEN = \"no\"\\n' >&3; fi\n",
            m = marker
        );
        let bin = write_script(dir.path(), "execd", &body);

        // empty_env() carries none of the host vars, mirroring Go's env.List().
        let env = empty_env();
        let vars = run_exec_d(&bin, &env).unwrap();
        unsafe { std::env::remove_var(marker) };

        // Go-faithful: child env is fully controlled by env, so the parent
        // marker is invisible. launcher-rs leaks it -> SEEN == "yes" -> FAILS.
        assert_eq!(
            vars.get("SEEN").map(String::as_str),
            Some("no"),
            "exec.d child saw a leaked parent env var (no env_clear)"
        );
    }
}
