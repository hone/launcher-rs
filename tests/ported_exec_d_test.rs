//! Ported from launcher-rust `src/launch/exec_d.rs::forwards_stdout_and_stderr`.
//!
//! launcher-rust's exec.d runner takes explicit `Out`/`Err` writers and forwards
//! the child's stdout/stderr to them (mirroring Go `lifecycle/launch/exec_d.go`).
//! launcher-rs's `run_exec_d(path, &LaunchEnv) -> Result<HashMap, _>` has NO
//! writer seam (no `Stdio::piped`, the child inherits the parent's stdio), so the
//! stream-forwarding behavior cannot be exercised against a private function. Per
//! rule 3 this port is downgraded to a binary_behavioral test that drives the
//! compiled launcher end-to-end: the launcher runs the layer's `exec.d/` binaries
//! before launching the process, the child inherits the launcher's stdio, and the
//! FD3 TOML vars are applied to the launch environment via `run_exec_d_in_dir`
//! (src/main.rs).

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::tempdir;

fn launcher_bin() -> PathBuf {
    Path::new("target/debug/launcher")
        .canonicalize()
        .expect("launcher binary not found at target/debug/launcher; run `cargo build` first")
}

fn make_executable(p: &Path) {
    let mut perms = fs::metadata(p).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(p, perms).unwrap();
}

/// Mirrors launcher-rust `forwards_stdout_and_stderr`
/// (launcher-rust/src/launch/exec_d.rs:253).
///
/// Go-correct intent: the exec.d binary's stdout is forwarded to the runner's
/// `Out` (the launcher's stdout) and its stderr to the runner's `Err` (the
/// launcher's stderr); the FD3 TOML var `A = "1"` is applied to the launch
/// environment.
///
/// launcher-rs has no writer seam, so it relies on inherited stdio: the child's
/// stdout/stderr land directly on the launcher's own stdout/stderr. We assert
/// both the stream forwarding AND that the FD3 var reaches the launched `env`
/// output.
///
/// GAP: launcher-rs `run_exec_d` has no `Out`/`Err` writer parameters (Go and
/// launcher-rust take explicit writers and route the child's streams through
/// them). This test pins only the inherited-stdio forwarding observable at the
/// binary boundary; the structural writer-routing seam itself remains untestable
/// internally. Expected outcome: pass.
#[test]
fn exec_d_stdout_stderr_forwarded() {
    let layers_temp = tempdir().unwrap();
    let layers_path = layers_temp.path();
    let app_temp = tempdir().unwrap();
    let app_path = app_temp.path();

    let config_dir = layers_path.join("config");
    fs::create_dir_all(&config_dir).unwrap();
    let metadata_content = r#"
[[processes]]
type = "web"
command = ["env"]
args = []
direct = true
default = true
buildpack-id = "my-bp"

[[buildpacks]]
id = "my-bp"
api = "0.12"
"#;
    fs::write(config_dir.join("metadata.toml"), metadata_content).unwrap();

    let layer_dir = layers_path.join("my-bp").join("test-layer");
    let exec_d_dir = layer_dir.join("exec.d");
    fs::create_dir_all(&exec_d_dir).unwrap();
    let script = exec_d_dir.join("01-streams.sh");
    fs::write(
        &script,
        "#!/bin/sh\necho out-line\necho err-line >&2\nprintf 'A = \"1\"\\n' >&3\n",
    )
    .unwrap();
    make_executable(&script);

    let mut cmd = Command::new(launcher_bin());
    cmd.env("CNB_PLATFORM_API", "0.15");
    cmd.env("CNB_LAYERS_DIR", layers_path.to_string_lossy().to_string());
    cmd.env("CNB_APP_DIR", app_path.to_string_lossy().to_string());
    cmd.arg("--").arg("env");

    let output = cmd.output().unwrap();
    assert!(
        output.status.success(),
        "launcher failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Go-faithful: exec.d stdout forwarded to the launcher's stdout.
    assert!(
        stdout.contains("out-line"),
        "expected exec.d stdout 'out-line' on launcher stdout; stdout={}",
        stdout
    );
    // Go-faithful: exec.d stderr forwarded to the launcher's stderr.
    assert!(
        stderr.contains("err-line"),
        "expected exec.d stderr 'err-line' on launcher stderr; stderr={}",
        stderr
    );
    // Go-faithful: FD3 var A=1 applied to the launched process environment.
    assert!(
        stdout.contains("A=1"),
        "expected FD3 var A=1 in launched env; stdout={}",
        stdout
    );
}
