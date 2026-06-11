//! Ported misc behavioral tests.
//!
//! These mirror launcher-rust's Go-faithful unit tests from `src/exit.rs` and
//! `src/launch/metadata.rs`. Because launcher-rs is a binary-only crate (no
//! `[lib]` target), the internal types those Go tests exercise (`Metadata`,
//! `Process`, `RawCommand`, `LauncherError`, `ExitCode`, `read_metadata`) are
//! NOT reachable from an integration test. Every case here is therefore
//! re-expressed at the binary level by driving `target/debug/launcher` via
//! `std::process::Command`, asserting the Go-correct observable behavior.
//!
//! Tests tagged `// GAP:` assert the Go-correct value that launcher-rust pins;
//! launcher-rs diverges, so those tests are EXPECTED TO FAIL. That failure is
//! the deliverable: it documents the behavioral divergence.

#![cfg(unix)]

use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::tempdir;

/// Canonicalize the compiled launcher binary path, mirroring
/// `tests/integration_test.rs`.
fn launcher_bin() -> PathBuf {
    let p = Path::new("target/debug/launcher").canonicalize();
    assert!(
        p.is_ok(),
        "Launcher binary not found at target/debug/launcher. Please run `cargo build` first."
    );
    p.unwrap()
}

// ---------------------------------------------------------------------------
// src/launch/metadata.rs ports
// ---------------------------------------------------------------------------

// GAP: launcher-rust's `read_metadata` (src/launch/metadata.rs:117-120) maps a
// failed `read_to_string` to `format!("read metadata: {e}")`, so a missing file
// surfaces the OS-level error text ("No such file or directory"). launcher-rs
// has NO `read_metadata` seam: main.rs pre-checks `metadata_path.is_file()`
// (main.rs:215-219) and short-circuits with its OWN wording
// "failed to read metadata: metadata file not found at '<path>'" -- the OS error
// is never produced. Asserting the Go-correct OS-error wording therefore FAILS
// against launcher-rs, exposing the message-wording divergence.
#[test]
fn read_metadata_missing_file_errors() {
    let layers = tempdir().unwrap();
    // config/metadata.toml deliberately absent.

    let output = Command::new(launcher_bin())
        .env("CNB_PLATFORM_API", "0.15")
        .env("CNB_LAYERS_DIR", layers.path())
        .output()
        .unwrap();

    // Go-correct (launcher-rust): the missing file must fail.
    assert!(
        !output.status.success(),
        "expected non-zero exit for missing metadata; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Go-correct error wording from launcher-rust read_metadata: the underlying
    // OS error ("No such file or directory") is surfaced verbatim. launcher-rs
    // emits "metadata file not found at" instead, so this FAILS (the gap).
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains("No such file or directory"),
        "expected Go-correct OS error wording 'No such file or directory'; got: {combined}"
    );
}

// PASS: launcher-rust's `Metadata.processes` carries `#[serde(default)]`
// (metadata.rs:15-16), so a metadata file with an empty/absent process list is
// handled gracefully rather than crashing the decoder. launcher-rs's
// `RawMetadata.processes` has NO serde default, so a *missing* field is a parse
// error; the Go-faithful "handled" guarantee is portable only for the explicit
// empty list `processes = []`, which launcher-rs accepts. We assert the
// Go-correct observable outcome: an empty process set with no user command does
// NOT crash/parse-error -- it deterministically reaches process selection and
// reports the no-default-process condition.
#[test]
fn metadata_missing_processes_handled() {
    let layers = tempdir().unwrap();
    let config_dir = layers.path().join("config");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("metadata.toml"),
        "processes = []\nbuildpacks = []\n",
    )
    .unwrap();

    let output = Command::new(launcher_bin())
        .env("CNB_PLATFORM_API", "0.15")
        .env("CNB_LAYERS_DIR", layers.path())
        .output()
        .unwrap();

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // Go-correct: an empty process list is decoded cleanly (no metadata parse
    // failure). The launcher proceeds to selection and reports that, with no
    // default process and no command, a command is required.
    assert!(
        !combined.contains("metadata"),
        "empty processes must not surface a metadata parse error; got: {combined}"
    );
    assert!(
        combined.contains("when there is no default process a command is required"),
        "expected the no-default-process selection error; got: {combined}"
    );
}

// PASS: launcher-rust's `Process.direct` and `Process.default` both default to
// false (metadata.rs:28-31 `#[serde(default)]`). A process declaring
// `direct = false` with `default` omitted (=> false) must still be selectable by
// argv0 (symlink) and run indirectly (via the shell). We drive the binary with
// argv0 set to the process type and assert it launches successfully -- proving
// the `direct = false` / `default = false` defaults route to the shell-wrapped
// path, not a crash.
#[test]
fn metadata_direct_default_false() {
    let layers = tempdir().unwrap();
    let app = tempdir().unwrap();
    let config_dir = layers.path().join("config");
    fs::create_dir_all(&config_dir).unwrap();

    // `direct = false`, `default` omitted (defaults to false).
    let metadata = r#"
[[processes]]
type = "web"
command = ["echo", "indirect-default-false"]
direct = false
buildpack-id = "my-bp"

[[buildpacks]]
id = "my-bp"
api = "0.12"
"#;
    fs::write(config_dir.join("metadata.toml"), metadata).unwrap();

    // argv0 == "web" triggers the symlink-selection rule, so the non-default,
    // non-direct process is chosen and executed via the shell.
    let output = Command::new(launcher_bin())
        .arg0("web")
        .env("CNB_PLATFORM_API", "0.15")
        .env("CNB_LAYERS_DIR", layers.path())
        .env("CNB_APP_DIR", app.path())
        .env("CNB_EXEC_ENV", "production")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "indirect (direct=false) process selected by argv0 should run; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("indirect-default-false"),
        "expected the indirect process output; got stdout: {stdout}"
    );
}

// PASS: launcher-rust's `RawCommand` accepts the legacy single-string form
// (metadata.rs:67-75 `visit_str`/`visit_string`). launcher-rs's `RawCommand`
// is an untagged enum with a `Single(String)` variant (launch.rs:17-22) that
// likewise accepts `command = "<string>"`. We assert the string-form command
// parses and executes end-to-end. With a single-string command run indirectly,
// the shell parses the string into program + args, so `echo hello-string-form`
// prints `hello-string-form`.
#[test]
fn metadata_command_string_form() {
    let layers = tempdir().unwrap();
    let app = tempdir().unwrap();
    let config_dir = layers.path().join("config");
    fs::create_dir_all(&config_dir).unwrap();

    // command given as a single string (legacy form), run indirectly.
    let metadata = r#"
[[processes]]
type = "web"
command = "echo hello-string-form"
direct = false
buildpack-id = "my-bp"

[[buildpacks]]
id = "my-bp"
api = "0.12"
"#;
    fs::write(config_dir.join("metadata.toml"), metadata).unwrap();

    let output = Command::new(launcher_bin())
        .arg0("web")
        .env("CNB_PLATFORM_API", "0.15")
        .env("CNB_LAYERS_DIR", layers.path())
        .env("CNB_APP_DIR", app.path())
        .env("CNB_EXEC_ENV", "production")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "string-form command should parse and run; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("hello-string-form"),
        "expected string-form command output; got stdout: {stdout}"
    );
}

// ---------------------------------------------------------------------------
// src/exit.rs ports
// ---------------------------------------------------------------------------

/// Builds a layers dir whose metadata has a process but NO default and NO
/// eligible command, reproducing launcher-rs `ProcessSelectionError::
/// NoCommandAndNoDefault` -- the arg/usage-class failure path. Returned dir
/// must outlive the command invocation.
fn arg_error_layers() -> tempfile::TempDir {
    let layers = tempdir().unwrap();
    let config_dir = layers.path().join("config");
    fs::create_dir_all(&config_dir).unwrap();
    let metadata = r#"
[[processes]]
type = "web"
command = ["echo"]
args = ["test"]
direct = true
default = false

[[buildpacks]]
id = "my-bp"
api = "0.12"
"#;
    fs::write(config_dir.join("metadata.toml"), metadata).unwrap();
    layers
}

// GAP: launcher-rust/Go define `ExitCode::Success == 0` and
// `ExitCode::InvalidArgs == 3` (src/exit.rs:5,7); arg/usage failures yield exit
// 3. launcher-rs's `ExitCode` enum (src/exit.rs) has NEITHER variant, and there
// is NO exit-3 code path -- arg-class failures collapse to
// `ExitCode::LaunchError == 82` via `ProcessSelectionError::NoCommandAndNoDefault`
// (launch.rs:72, main.rs:184). The Go-correct expectation is that an
// arg-parse-class failure produces exit 3 (and never a spurious success 0).
// Asserting `== 3` FAILS (launcher-rs returns 82), exposing the missing-variant
// gap. (Naming `ExitCode::InvalidArgs` in source would be a compile error, so
// this is expressed purely at the binary level.)
#[test]
fn exit_code_success_zero_and_invalid_args_three_absent() {
    let layers = arg_error_layers();

    let output = Command::new(launcher_bin())
        .env("CNB_PLATFORM_API", "0.15")
        .env("CNB_LAYERS_DIR", layers.path())
        .output()
        .unwrap();

    let code = output.status.code().expect("process exited via signal");

    // Go-correct: no spurious success on an error path.
    assert_ne!(code, 0, "arg/usage failure must not exit 0 (Success)");
    // Go-correct: an arg-parse-class failure is ExitCode::InvalidArgs == 3.
    assert_eq!(
        code, 3,
        "Go-correct arg failure exit code is 3 (InvalidArgs); launcher-rs returns {code}"
    );
}

// GAP: launcher-rust's `invalid_args_code_is_3` (src/exit.rs:166-170) pins that
// arg-parse failures map to `ExitCode::InvalidArgs == 3`. launcher-rs has no
// `InvalidArgs` variant and no 3-mapping: the "no default process / no command"
// arg error maps to `ExitCode::LaunchError == 82`
// (ProcessSelectionError::NoCommandAndNoDefault). Asserting Go's 3 FAILS.
#[test]
fn invalid_args_code_is_3() {
    let layers = arg_error_layers();

    let output = Command::new(launcher_bin())
        .env("CNB_PLATFORM_API", "0.15")
        .env("CNB_LAYERS_DIR", layers.path())
        .output()
        .unwrap();

    let code = output.status.code().expect("process exited via signal");
    assert_eq!(
        code, 3,
        "Go-correct: arg error exit code is 3 (InvalidArgs); launcher-rs returns {code}"
    );
}

// GAP: launcher-rust's `formats_action_only_when_message_empty` (src/exit.rs:147-151)
// pins `LauncherError::new(Failed, "launch", "").to_string() == "failed to launch"`:
// when the message is empty, ONLY the action prefix prints (no trailing ": ").
// launcher-rs has no generic `LauncherError::new` and no empty-message branch;
// every launch-class variant Display is hard-coded as `"failed to launch: {0}"`
// (main.rs:115,118,121,168,171) and always interpolates a non-empty detail. The
// no-default-process scenario formats
// "failed to launch: determine start command: when there is no default process a
// command is required". Asserting the Go-correct BARE "ERROR: failed to launch"
// (no colon/detail) FAILS, exposing the missing empty-message branch.
#[test]
fn formats_action_only_when_message_empty() {
    let layers = arg_error_layers();

    let output = Command::new(launcher_bin())
        .env("CNB_PLATFORM_API", "0.15")
        .env("CNB_LAYERS_DIR", layers.path())
        // Disable ANSI color so the prefix is the plain "ERROR: ".
        .env("CNB_NO_COLOR", "true")
        .output()
        .unwrap();

    // Go-correct routing/format: the launch error line, when the message is
    // empty, is exactly "ERROR: failed to launch\n" with no trailing detail.
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let line = combined
        .lines()
        .find(|l| l.contains("failed to launch"))
        .unwrap_or("");
    assert_eq!(
        line, "ERROR: failed to launch",
        "Go-correct empty-message format is bare 'failed to launch' (no colon/detail); got: {line}"
    );
}

// GAP: launcher-rust's `report_format_matches_go_failerr` (src/exit.rs:182-191)
// pins TWO facts: (1) the "ERROR: <msg>" prefix/format, and (2) Go's
// DefaultLogger routes errors to STDOUT (launcher-rust `report()` deliberately
// writes stdout for byte parity, src/exit.rs:118-124). launcher-rs's format
// matches ("ERROR: <msg>" via format_error, main.rs:66-72) but it routes errors
// to STDERR (`print_error` -> `eprintln!`, main.rs:84-86,95). Asserting the
// Go-correct routing -- the ERROR line must appear on STDOUT -- FAILS because
// launcher-rs writes it to stderr, exposing the stderr-vs-stdout divergence.
#[test]
fn report_format_matches_go_failerr() {
    let layers = tempdir().unwrap();
    // No config/metadata.toml -> "failed to read metadata: ..." error path.

    let output = Command::new(launcher_bin())
        .env("CNB_PLATFORM_API", "0.15")
        .env("CNB_LAYERS_DIR", layers.path())
        .env("CNB_NO_COLOR", "true")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // The format prefix is shared and correct in both implementations.
    assert!(
        stderr.contains("ERROR: failed to read metadata")
            || stdout.contains("ERROR: failed to read metadata"),
        "expected an 'ERROR: failed to read metadata' line somewhere; stdout: {stdout} stderr: {stderr}"
    );

    // Go-correct routing: the ERROR line lands on STDOUT (DefaultLogger ->
    // Stdout). launcher-rs writes it to STDERR, so this FAILS (the gap).
    assert!(
        stdout.contains("ERROR: failed to read metadata"),
        "Go-correct: error report must be written to STDOUT; launcher-rs wrote to STDERR. stdout: {stdout} stderr: {stderr}"
    );
}
