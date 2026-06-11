//! Ported behavioral tests mirroring launcher-rust's tests/integration.rs and
//! tests/golden_test.rs.
//!
//! launcher-rs is a binary-only crate (no [lib]), so every launcher-rust test
//! that imported the `launcher::` library API is re-expressed here as a
//! BEHAVIORAL test that drives the compiled `target/debug/launcher` binary via
//! std::process::Command. We canonicalize the binary path the same way the
//! existing tests/integration_test.rs does, and use
//! std::os::unix::process::CommandExt::arg0() to set argv0 for symlink-style
//! process selection (no assert_cmd dev-dep; tempfile only).
//!
//! Each test asserts the Go-CORRECT value (what launcher-rust asserts). The
//! three tests marked `// GAP:` are expected to FAIL against launcher-rs; that
//! failure is the deliverable and documents the divergence.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::tempdir;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve the compiled launcher binary, canonicalized (same pattern as the
/// existing tests/integration_test.rs).
fn launcher_bin() -> PathBuf {
    Path::new("target/debug/launcher")
        .canonicalize()
        .expect("launcher binary not found at target/debug/launcher; run cargo build first")
}

/// Build a fresh launcher Command with a clean, deterministic env that contains
/// only the listed CNB_* vars plus a basic PATH. Each test layers in extras.
fn base_cmd(layers: &Path, app: &Path) -> Command {
    let mut cmd = Command::new(launcher_bin());
    cmd.env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", "/tmp")
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("TERM", "dumb")
        .env("CNB_NO_COLOR", "true")
        .env("CNB_LAYERS_DIR", layers)
        .env("CNB_APP_DIR", app);
    cmd
}

/// Write `metadata.toml` under `<layers>/config/`.
fn write_metadata(layers: &Path, metadata_toml: &str) {
    let config = layers.join("config");
    fs::create_dir_all(&config).unwrap();
    fs::write(config.join("metadata.toml"), metadata_toml).unwrap();
}

/// Create `<layers>/<escaped-bp-id>/<layer>/` and return the layer dir.
fn make_layer(layers: &Path, escaped_bp: &str, layer: &str) -> PathBuf {
    let p = layers.join(escaped_bp).join(layer);
    fs::create_dir_all(&p).unwrap();
    p
}

/// Write an executable script (mode 0o755), creating parent dirs as needed.
fn write_executable(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, body).unwrap();
    let mut perm = fs::metadata(path).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(path, perm).unwrap();
}

fn combined(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr)
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Return the PATH= line value (after `PATH=`) from an `env`/`printenv` dump.
fn path_line(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .find_map(|l| l.strip_prefix("PATH=").map(str::to_string))
}

// ===========================================================================
// 1. missing_platform_api_exits_11
//    (integration.rs:67 missing_platform_api_exits_11 + golden missing-platform-api)
// ===========================================================================
#[test]
fn missing_platform_api_exits_11() {
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    write_metadata(&layers, "");

    let mut cmd = base_cmd(&layers, &app);
    cmd.env_remove("CNB_PLATFORM_API");
    let output = cmd.output().expect("spawn launcher");

    assert_eq!(output.status.code(), Some(11));
    let c = combined(&output);
    assert!(
        c.contains("please set 'CNB_PLATFORM_API'"),
        "expected CNB_PLATFORM_API hint; got:\n{c}"
    );
}

// ===========================================================================
// 2. platform_api_empty_exits_11
//    (golden missing-platform-api uses the empty-string knob)
// ===========================================================================
#[test]
fn platform_api_empty_exits_11() {
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    write_metadata(&layers, "");

    let mut cmd = base_cmd(&layers, &app);
    cmd.env("CNB_PLATFORM_API", "");
    let output = cmd.output().expect("spawn launcher");

    assert_eq!(output.status.code(), Some(11));
    let c = combined(&output);
    assert!(
        c.contains("please set 'CNB_PLATFORM_API'"),
        "expected CNB_PLATFORM_API hint; got:\n{c}"
    );
}

// ===========================================================================
// 3. platform_api_unparseable_exits_11
//    (api verify_platform_api_unparseable, observed through the binary)
// ===========================================================================
#[test]
fn platform_api_unparseable_exits_11() {
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    write_metadata(&layers, "");

    let mut cmd = base_cmd(&layers, &app);
    cmd.env("CNB_PLATFORM_API", "not-a-version");
    let output = cmd.output().expect("spawn launcher");

    assert_eq!(output.status.code(), Some(11));
    let c = combined(&output);
    assert!(
        c.contains("failed to parse platform API 'not-a-version'"),
        "expected parse-platform-API error; got:\n{c}"
    );
}

// ===========================================================================
// 4. buildpack_api_incompatible_exits_12
//    (integration.rs:741 exit_code_for_unsupported_buildpack_api)
// ===========================================================================
#[test]
fn buildpack_api_incompatible_exits_12() {
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    let metadata = r#"
[[processes]]
type = "web"
command = "echo"
direct = true

[[buildpacks]]
api = "99.0"
id = "bad/bp"
"#;
    write_metadata(&layers, metadata);

    let mut cmd = base_cmd(&layers, &app);
    cmd.env("CNB_PLATFORM_API", "0.12");
    let output = cmd.output().expect("spawn launcher");

    assert_eq!(
        output.status.code(),
        Some(12),
        "expected exit 12; combined:\n{}",
        combined(&output)
    );
    let c = combined(&output);
    assert!(c.contains("incompatible"), "expected 'incompatible'; got:\n{c}");
    assert!(c.contains("bad/bp"), "expected buildpack id in error; got:\n{c}");
}

// ===========================================================================
// 5. no_default_no_cmd_exits_82
//    (integration.rs:91 empty_cmd_no_default_process_exits_82)
//
//    argv0 basename is 'launcher' (no arg0 override) and there are no user
//    args, so nothing is selected -> NoCommandAndNoDefault (exit 82).
// ===========================================================================
#[test]
fn no_default_no_cmd_exits_82() {
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    let metadata = r#"
processes = []

[[buildpacks]]
id = "samples/dummy"
api = "0.10"
"#;
    write_metadata(&layers, metadata);

    let mut cmd = base_cmd(&layers, &app);
    cmd.env("CNB_PLATFORM_API", "0.15");
    let output = cmd.output().expect("spawn launcher");

    assert_eq!(
        output.status.code(),
        Some(82),
        "expected exit 82; combined:\n{}",
        combined(&output)
    );
    let c = combined(&output);
    assert!(
        c.contains("when there is no default process a command is required"),
        "expected no-default-process message; got:\n{c}"
    );
}

// ===========================================================================
// 6. direct_process_runs_and_exits_0
//    (integration.rs:119 direct_exec_double_dash_runs_true)
// ===========================================================================
#[test]
fn direct_process_runs_and_exits_0() {
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    let metadata = r#"
processes = []

[[buildpacks]]
id = "samples/dummy"
api = "0.10"
"#;
    write_metadata(&layers, metadata);

    let mut cmd = base_cmd(&layers, &app);
    cmd.env("CNB_PLATFORM_API", "0.15");
    cmd.arg("--").arg("/usr/bin/true");
    let output = cmd.output().expect("spawn launcher");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout=\n{}\nstderr=\n{}",
        stdout_of(&output),
        stderr_of(&output)
    );
}

// ===========================================================================
// 7. shell_process_sources_profile
//    (integration.rs:279 profile_d_exports_var_visible_to_user_process)
//
//    argv0 'web' (symlink semantics via arg0) -> selects the web process which
//    is direct=false. profile.d export reaches the /usr/bin/env child.
// ===========================================================================
#[test]
fn shell_process_sources_profile() {
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    let metadata = r#"
[[processes]]
type = "web"
command = ["/usr/bin/env"]
args = []
direct = false
buildpack-id = "samples/dummy"

[[buildpacks]]
id = "samples/dummy"
api = "0.10"
"#;
    write_metadata(&layers, metadata);

    let layer = make_layer(&layers, "samples_dummy", "mylayer");
    fs::create_dir_all(layer.join("profile.d")).unwrap();
    fs::write(layer.join("profile.d/00-greet.sh"), "export HELLO=world\n").unwrap();

    let mut cmd = base_cmd(&layers, &app);
    cmd.arg0("web").env("CNB_PLATFORM_API", "0.15");
    let output = cmd.output().expect("spawn launcher");

    assert!(
        output.status.success(),
        "launcher failed: stdout=\n{}\nstderr=\n{}",
        stdout_of(&output),
        stderr_of(&output)
    );
    let stdout = stdout_of(&output);
    assert!(
        stdout.lines().any(|l| l == "HELLO=world"),
        "expected HELLO=world from profile.d; got:\n{stdout}"
    );
}

// ===========================================================================
// 8. env_launch_var_propagates
//    (integration.rs:155 shell_mode_env_launch_propagates_foo /
//     env_launch_dir_processing_with_fixtures)
// ===========================================================================
#[test]
fn env_launch_var_propagates() {
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    let metadata = r#"
[[processes]]
type = "web"
command = ["/usr/bin/env"]
args = []
direct = true
buildpack-id = "samples/dummy"

[[buildpacks]]
id = "samples/dummy"
api = "0.10"
"#;
    write_metadata(&layers, metadata);

    let layer = make_layer(&layers, "samples_dummy", "mylayer");
    fs::create_dir_all(layer.join("env.launch")).unwrap();
    fs::write(layer.join("env.launch/FOO"), "hello-from-launch").unwrap();

    let mut cmd = base_cmd(&layers, &app);
    cmd.arg0("web").env("CNB_PLATFORM_API", "0.15");
    let output = cmd.output().expect("spawn launcher");

    assert!(
        output.status.success(),
        "launcher failed: stdout=\n{}\nstderr=\n{}",
        stdout_of(&output),
        stderr_of(&output)
    );
    let stdout = stdout_of(&output);
    assert!(
        stdout.lines().any(|l| l == "FOO=hello-from-launch"),
        "expected FOO=hello-from-launch in env output; got:\n{stdout}"
    );
}

// ===========================================================================
// 9. exec_d_var_visible_to_process
//    (integration.rs:218 exec_d_emits_var_visible_to_user_process)
// ===========================================================================
#[test]
fn exec_d_var_visible_to_process() {
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    let metadata = r#"
[[processes]]
type = "web"
command = ["/usr/bin/env"]
args = []
direct = true
buildpack-id = "samples/dummy"

[[buildpacks]]
id = "samples/dummy"
api = "0.10"
"#;
    write_metadata(&layers, metadata);

    let layer = make_layer(&layers, "samples_dummy", "mylayer");
    write_executable(
        &layer.join("exec.d/00-emit"),
        "#!/bin/sh\nprintf 'BAR = \"baz\"\\n' >&3\n",
    );

    let mut cmd = base_cmd(&layers, &app);
    cmd.arg0("web").env("CNB_PLATFORM_API", "0.15");
    let output = cmd.output().expect("spawn launcher");

    assert!(
        output.status.success(),
        "launcher failed: stdout=\n{}\nstderr=\n{}",
        stdout_of(&output),
        stderr_of(&output)
    );
    let stdout = stdout_of(&output);
    assert!(
        stdout.lines().any(|l| l == "BAR=baz"),
        "expected BAR=baz from exec.d; got:\n{stdout}"
    );
}

// ===========================================================================
// 10. argv0_symlink_selects_process
//    (integration.rs:556 process_selection_default_type, via argv0 basename)
// ===========================================================================
#[test]
fn argv0_symlink_selects_process() {
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    let metadata = r#"
[[processes]]
type = "web"
command = ["/bin/echo", "hello"]
args = []
direct = true
default = true
buildpack-id = "samples/dummy"

[[buildpacks]]
id = "samples/dummy"
api = "0.9"
"#;
    write_metadata(&layers, metadata);

    let mut cmd = base_cmd(&layers, &app);
    cmd.arg0("web").env("CNB_PLATFORM_API", "0.12");
    let output = cmd.output().expect("spawn launcher");

    assert!(
        output.status.success(),
        "launcher failed: stdout=\n{}\nstderr=\n{}",
        stdout_of(&output),
        stderr_of(&output)
    );
    assert_eq!(stdout_of(&output).trim_end(), "hello");
}

// ===========================================================================
// 11. user_args_append_bp_lt09
//    (integration.rs:481 test_resolve_process_args_replacement, bp < 0.9 path)
//
//    Platform >= 0.10, single-entry command, buildpack API < 0.9: user args are
//    APPENDED to the process's own args.
// ===========================================================================
#[test]
fn user_args_append_bp_lt09() {
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    let metadata = r#"
[[processes]]
type = "web"
command = "/bin/echo"
args = ["server.js"]
direct = true
buildpack-id = "samples/dummy"

[[buildpacks]]
id = "samples/dummy"
api = "0.8"
"#;
    write_metadata(&layers, metadata);

    let mut cmd = base_cmd(&layers, &app);
    cmd.arg0("web").env("CNB_PLATFORM_API", "0.12").arg("user-arg");
    let output = cmd.output().expect("spawn launcher");

    assert!(
        output.status.success(),
        "launcher failed: stdout=\n{}\nstderr=\n{}",
        stdout_of(&output),
        stderr_of(&output)
    );
    // bp < 0.9: defaults kept AND user args appended.
    assert_eq!(stdout_of(&output).trim_end(), "server.js user-arg");
}

// ===========================================================================
// 12. user_args_replace_bp_ge09
//    (integration.rs:341 arg_merge_replaces_overridable_args_on_bp_ge_09 /
//     test_resolve_process_args_replacement bp >= 0.9 path)
//
//    Platform >= 0.10, single-entry command, buildpack API >= 0.9: user args
//    REPLACE the process's overridable args.
// ===========================================================================
#[test]
fn user_args_replace_bp_ge09() {
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    let metadata = r#"
[[processes]]
type = "web"
command = "/bin/echo"
args = ["override-me"]
direct = true
buildpack-id = "samples/dummy"

[[buildpacks]]
id = "samples/dummy"
api = "0.10"
"#;
    write_metadata(&layers, metadata);

    let mut cmd = base_cmd(&layers, &app);
    cmd.arg0("web")
        .env("CNB_PLATFORM_API", "0.15")
        .arg("replaced1")
        .arg("replaced2");
    let output = cmd.output().expect("spawn launcher");

    assert!(
        output.status.success(),
        "launcher failed: stdout=\n{}\nstderr=\n{}",
        stdout_of(&output),
        stderr_of(&output)
    );
    // bp >= 0.9: user args replace the overridable default 'override-me'.
    assert_eq!(stdout_of(&output).trim_end(), "replaced1 replaced2");
}

// ===========================================================================
// 13. cnb_app_dir_missing_errors
//    (integration.rs:820 cnb_app_dir_defaults_to_workspace; here we set
//     CNB_APP_DIR explicitly to a missing dir to assert the chdir failure
//     deterministically without depending on the host /workspace.)
// ===========================================================================
#[test]
fn cnb_app_dir_missing_errors() {
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app_missing = tmp.path().join("_app_does_not_exist");
    let metadata = r#"
[[processes]]
type = "web"
command = "/bin/echo"
args = ["hello"]
direct = true
default = true

[[buildpacks]]
api = "0.9"
id = "test/bp"
"#;
    write_metadata(&layers, metadata);

    let mut cmd = base_cmd(&layers, &app_missing);
    cmd.env("CNB_PLATFORM_API", "0.12")
        .args(["--", "/bin/echo", "hello"]);
    let output = cmd.output().expect("spawn launcher");

    assert!(
        !output.status.success(),
        "expected non-zero exit when app dir does not exist"
    );
    let c = combined(&output);
    assert!(
        c.contains("change to app directory"),
        "expected chdir error; got:\n{c}"
    );
}

// ===========================================================================
// 14. direct_marker_runs_user_command
//    (integration.rs:581 process_selection_double_dash_direct)
// ===========================================================================
#[test]
fn direct_marker_runs_user_command() {
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    let metadata = r#"
processes = []

[[buildpacks]]
id = "samples/dummy"
api = "0.12"
"#;
    write_metadata(&layers, metadata);

    let mut cmd = base_cmd(&layers, &app);
    cmd.env("CNB_PLATFORM_API", "0.12")
        .args(["--", "/bin/echo", "direct_output"]);
    let output = cmd.output().expect("spawn launcher");

    assert!(
        output.status.success(),
        "launcher failed: stdout=\n{}\nstderr=\n{}",
        stdout_of(&output),
        stderr_of(&output)
    );
    assert_eq!(stdout_of(&output).trim_end(), "direct_output");
}

// ===========================================================================
// 15. env_excludelist_not_visible
//    (integration.rs:457 env_construction_with_exclusions_and_path_stripping)
//
//    LAUNCH_ENV_EXCLUDELIST CNB_* vars are dropped from the child env; a
//    non-excluded custom var survives.
// ===========================================================================
#[test]
fn env_excludelist_not_visible() {
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    let metadata = r#"
processes = []

[[buildpacks]]
id = "samples/dummy"
api = "0.12"
"#;
    write_metadata(&layers, metadata);

    let mut cmd = base_cmd(&layers, &app);
    cmd.env("CNB_PLATFORM_API", "0.12")
        .env("MYVAR", "keep")
        .args(["--", "/usr/bin/env"]);
    let output = cmd.output().expect("spawn launcher");

    assert!(
        output.status.success(),
        "launcher failed: stdout=\n{}\nstderr=\n{}",
        stdout_of(&output),
        stderr_of(&output)
    );
    let stdout = stdout_of(&output);
    assert!(
        !stdout.lines().any(|l| l.starts_with("CNB_PLATFORM_API=")),
        "CNB_PLATFORM_API must not leak into child env; got:\n{stdout}"
    );
    assert!(
        !stdout.lines().any(|l| l.starts_with("CNB_LAYERS_DIR=")),
        "CNB_LAYERS_DIR must not leak into child env; got:\n{stdout}"
    );
    assert!(
        stdout.lines().any(|l| l == "MYVAR=keep"),
        "expected non-excluded MYVAR=keep to survive; got:\n{stdout}"
    );
}

// ===========================================================================
// 16. profile_d_sourced_before_process
//    (integration.rs:624 profile_collection_order_from_fixtures: .profile last)
//
//    A var set by a buildpack profile.d script is OVERRIDDEN by the app
//    /.profile, proving the app .profile is sourced LAST.
// ===========================================================================
#[test]
fn profile_d_sourced_before_process() {
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    let metadata = r#"
[[processes]]
type = "web"
command = ["/usr/bin/env"]
args = []
direct = false
buildpack-id = "samples/dummy"

[[buildpacks]]
id = "samples/dummy"
api = "0.10"
"#;
    write_metadata(&layers, metadata);

    let layer = make_layer(&layers, "samples_dummy", "mylayer");
    fs::create_dir_all(layer.join("profile.d")).unwrap();
    fs::write(
        layer.join("profile.d/00-setup.sh"),
        "export ORDER=from_profiled\n",
    )
    .unwrap();
    fs::write(app.join(".profile"), "export ORDER=from_dotprofile\n").unwrap();

    let mut cmd = base_cmd(&layers, &app);
    cmd.arg0("web").env("CNB_PLATFORM_API", "0.15");
    let output = cmd.output().expect("spawn launcher");

    assert!(
        output.status.success(),
        "launcher failed: stdout=\n{}\nstderr=\n{}",
        stdout_of(&output),
        stderr_of(&output)
    );
    let stdout = stdout_of(&output);
    assert!(
        stdout.lines().any(|l| l == "ORDER=from_dotprofile"),
        "app .profile should source last (override profile.d); got:\n{stdout}"
    );
}

// ===========================================================================
// 17. layer_bin_on_path
//    (integration.rs:518 add_root_dir_with_fixture_layers)
//
//    A layer dir containing bin/ prepends that bin to PATH (and lib/ to
//    LD_LIBRARY_PATH). Observed via direct /usr/bin/env.
// ===========================================================================
#[test]
fn layer_bin_on_path() {
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    let metadata = r#"
processes = []

[[buildpacks]]
id = "samples/dummy"
api = "0.10"
"#;
    write_metadata(&layers, metadata);

    let layer = make_layer(&layers, "samples_dummy", "layer1");
    fs::create_dir_all(layer.join("bin")).unwrap();
    fs::create_dir_all(layer.join("lib")).unwrap();
    // add_root_dir canonicalizes the layer dir; compute expected accordingly.
    let canon_bin = fs::canonicalize(layer.join("bin"))
        .unwrap()
        .to_string_lossy()
        .into_owned();

    let mut cmd = base_cmd(&layers, &app);
    cmd.env("CNB_PLATFORM_API", "0.12")
        .args(["--", "/usr/bin/env"]);
    let output = cmd.output().expect("spawn launcher");

    assert!(
        output.status.success(),
        "launcher failed: stdout=\n{}\nstderr=\n{}",
        stdout_of(&output),
        stderr_of(&output)
    );
    let stdout = stdout_of(&output);
    let path = path_line(&stdout).unwrap_or_else(|| panic!("no PATH= line; got:\n{stdout}"));
    let first = path.split(':').next().unwrap_or("");
    assert_eq!(
        first, canon_bin,
        "layer bin should be first PATH entry; PATH={path}"
    );
    assert!(
        path.split(':').any(|e| e == "/usr/bin"),
        "original PATH entries should remain; PATH={path}"
    );
}

// ===========================================================================
// 18. multiple_buildpacks_env_order
//    (integration.rs ordering coverage: later buildpack overrides earlier)
//
//    Two buildpacks each set FOO via env.launch. main.rs applies buildpacks in
//    metadata order; the SECOND buildpack's Override wins.
// ===========================================================================
#[test]
fn multiple_buildpacks_env_order() {
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    let metadata = r#"
[[processes]]
type = "web"
command = ["/usr/bin/env"]
args = []
direct = true
buildpack-id = "aaa/first"

[[buildpacks]]
id = "aaa/first"
api = "0.10"

[[buildpacks]]
id = "zzz/second"
api = "0.10"
"#;
    write_metadata(&layers, metadata);

    let layer_a = make_layer(&layers, "aaa_first", "l1");
    fs::create_dir_all(layer_a.join("env.launch")).unwrap();
    fs::write(layer_a.join("env.launch/FOO"), "from-first").unwrap();

    let layer_z = make_layer(&layers, "zzz_second", "l1");
    fs::create_dir_all(layer_z.join("env.launch")).unwrap();
    fs::write(layer_z.join("env.launch/FOO"), "from-second").unwrap();

    let mut cmd = base_cmd(&layers, &app);
    cmd.arg0("web").env("CNB_PLATFORM_API", "0.15");
    let output = cmd.output().expect("spawn launcher");

    assert!(
        output.status.success(),
        "launcher failed: stdout=\n{}\nstderr=\n{}",
        stdout_of(&output),
        stderr_of(&output)
    );
    let stdout = stdout_of(&output);
    assert!(
        stdout.lines().any(|l| l == "FOO=from-second"),
        "later buildpack Override should win; got:\n{stdout}"
    );
}

// ===========================================================================
// 19. env_launch_type_specific_scoping
//    (integration.rs:697 env_launch_process_specific_dir_processing)
//
//    <layer>/env.launch/<type>/WEB_VAR applies only for the matching process
//    type.
// ===========================================================================
#[test]
fn env_launch_type_specific_scoping() {
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    let metadata = r#"
[[processes]]
type = "web"
command = ["/usr/bin/env"]
args = []
direct = true
buildpack-id = "samples/dummy"

[[buildpacks]]
id = "samples/dummy"
api = "0.10"
"#;
    write_metadata(&layers, metadata);

    let layer = make_layer(&layers, "samples_dummy", "mylayer");
    let web_env = layer.join("env.launch").join("web");
    fs::create_dir_all(&web_env).unwrap();
    fs::write(web_env.join("WEB_VAR"), "web_value").unwrap();

    let mut cmd = base_cmd(&layers, &app);
    cmd.arg0("web").env("CNB_PLATFORM_API", "0.15");
    let output = cmd.output().expect("spawn launcher");

    assert!(
        output.status.success(),
        "launcher failed: stdout=\n{}\nstderr=\n{}",
        stdout_of(&output),
        stderr_of(&output)
    );
    let stdout = stdout_of(&output);
    assert!(
        stdout.lines().any(|l| l == "WEB_VAR=web_value"),
        "expected per-type WEB_VAR=web_value; got:\n{stdout}"
    );
}

// ===========================================================================
// 20. root_dir_before_env_files
//    (ordering: add_root_dir runs before add_env_dir in main.rs, so an env
//     file that prepends to PATH lands BEFORE the layer bin in the final list.)
//
//    The layer bin/ is added first (PATH = <bin>:<orig>); then env/PATH.prepend
//    with PATH.delim=":" prepends "/extra/bin" -> "/extra/bin:<bin>:<orig>".
// ===========================================================================
#[test]
fn root_dir_before_env_files() {
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    let metadata = r#"
processes = []

[[buildpacks]]
id = "samples/dummy"
api = "0.10"
"#;
    write_metadata(&layers, metadata);

    let layer = make_layer(&layers, "samples_dummy", "layer1");
    fs::create_dir_all(layer.join("bin")).unwrap();
    let canon_bin = fs::canonicalize(layer.join("bin"))
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let env_dir = layer.join("env");
    fs::create_dir_all(&env_dir).unwrap();
    fs::write(env_dir.join("PATH.prepend"), "/extra/bin").unwrap();
    fs::write(env_dir.join("PATH.delim"), ":").unwrap();

    let mut cmd = base_cmd(&layers, &app);
    cmd.env("CNB_PLATFORM_API", "0.12")
        .args(["--", "/usr/bin/env"]);
    let output = cmd.output().expect("spawn launcher");

    assert!(
        output.status.success(),
        "launcher failed: stdout=\n{}\nstderr=\n{}",
        stdout_of(&output),
        stderr_of(&output)
    );
    let stdout = stdout_of(&output);
    let path = path_line(&stdout).unwrap_or_else(|| panic!("no PATH= line; got:\n{stdout}"));
    let entries: Vec<&str> = path.split(':').collect();
    assert_eq!(
        entries.first().copied(),
        Some("/extra/bin"),
        "env PATH.prepend should be first; PATH={path}"
    );
    assert!(
        entries.iter().any(|e| *e == canon_bin),
        "layer bin (added before env files) should still be present; PATH={path}"
    );
}

// ===========================================================================
// 21. exec_d_overrides_static_env
//    (exec.d runs AFTER env files in main.rs, so its emitted value wins over a
//     static env.launch value for the same key.)
// ===========================================================================
#[test]
fn exec_d_overrides_static_env() {
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    let metadata = r#"
[[processes]]
type = "web"
command = ["/usr/bin/env"]
args = []
direct = true
buildpack-id = "samples/dummy"

[[buildpacks]]
id = "samples/dummy"
api = "0.10"
"#;
    write_metadata(&layers, metadata);

    let layer = make_layer(&layers, "samples_dummy", "mylayer");
    fs::create_dir_all(layer.join("env.launch")).unwrap();
    fs::write(layer.join("env.launch/SHARED"), "static-value").unwrap();
    write_executable(
        &layer.join("exec.d/00-emit"),
        "#!/bin/sh\nprintf 'SHARED = \"dynamic-value\"\\n' >&3\n",
    );

    let mut cmd = base_cmd(&layers, &app);
    cmd.arg0("web").env("CNB_PLATFORM_API", "0.15");
    let output = cmd.output().expect("spawn launcher");

    assert!(
        output.status.success(),
        "launcher failed: stdout=\n{}\nstderr=\n{}",
        stdout_of(&output),
        stderr_of(&output)
    );
    let stdout = stdout_of(&output);
    assert!(
        stdout.lines().any(|l| l == "SHARED=dynamic-value"),
        "exec.d value should override static env.launch value; got:\n{stdout}"
    );
}

// ===========================================================================
// 22. direct_argv0_unresolved_name
//    (argv0 basename matches no process type; with user args + '--' the
//     launcher falls through to a direct user command.)
// ===========================================================================
#[test]
fn direct_argv0_unresolved_name() {
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    let metadata = r#"
[[processes]]
type = "web"
command = ["/bin/echo", "from-web"]
args = []
direct = true
buildpack-id = "samples/dummy"

[[buildpacks]]
id = "samples/dummy"
api = "0.10"
"#;
    write_metadata(&layers, metadata);

    let mut cmd = base_cmd(&layers, &app);
    // argv0 basename 'not-a-process-type' matches nothing -> user command path.
    cmd.arg0("not-a-process-type")
        .env("CNB_PLATFORM_API", "0.15")
        .args(["--", "/bin/echo", "user_command_ran"]);
    let output = cmd.output().expect("spawn launcher");

    assert!(
        output.status.success(),
        "launcher failed: stdout=\n{}\nstderr=\n{}",
        stdout_of(&output),
        stderr_of(&output)
    );
    assert_eq!(stdout_of(&output).trim_end(), "user_command_ran");
}

// ===========================================================================
// 23. working_dir_honored
//    (a process with working-dir set runs in that directory; observed via
//     /bin/pwd. Direct exec chdir's to working_directory before exec.)
// ===========================================================================
#[test]
fn working_dir_honored() {
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    let work = tmp.path().join("_work");
    fs::create_dir_all(&app).unwrap();
    fs::create_dir_all(&work).unwrap();
    let canon_work = fs::canonicalize(&work).unwrap().to_string_lossy().into_owned();

    let metadata = format!(
        r#"
[[processes]]
type = "web"
command = ["/bin/pwd"]
args = []
direct = true
buildpack-id = "samples/dummy"
working-dir = "{}"

[[buildpacks]]
id = "samples/dummy"
api = "0.10"
"#,
        canon_work
    );
    write_metadata(&layers, &metadata);

    let mut cmd = base_cmd(&layers, &app);
    cmd.arg0("web").env("CNB_PLATFORM_API", "0.15");
    let output = cmd.output().expect("spawn launcher");

    assert!(
        output.status.success(),
        "launcher failed: stdout=\n{}\nstderr=\n{}",
        stdout_of(&output),
        stderr_of(&output)
    );
    assert_eq!(
        stdout_of(&output).trim_end(),
        canon_work,
        "process should run in its configured working-dir"
    );
}

// ===========================================================================
// 24. no_color_respected
//    (main.rs: CNB_NO_COLOR=true -> error printed as plain "ERROR: ..." with no
//     ANSI escape sequence.)
// ===========================================================================
#[test]
fn no_color_respected() {
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    write_metadata(&layers, "");

    let mut cmd = base_cmd(&layers, &app);
    cmd.env("CNB_NO_COLOR", "true")
        .env_remove("CNB_PLATFORM_API");
    let output = cmd.output().expect("spawn launcher");

    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("ERROR: "),
        "expected plain 'ERROR: ' prefix; got:\n{stderr}"
    );
    assert!(
        !stderr.contains('\u{1b}'),
        "CNB_NO_COLOR=true must suppress ANSI escape sequences; got:\n{stderr}"
    );
}

// ===========================================================================
// 25. process_not_found_named_exits_82
//    (integration_test.rs:188 test_integration_process_not_found pattern:
//     a named process exists but is not default, argv0 'launcher', no args ->
//     NoCommandAndNoDefault, exit 82.)
// ===========================================================================
#[test]
fn process_not_found_named_exits_82() {
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    let metadata = r#"
[[processes]]
type = "web"
command = ["echo"]
args = ["test"]
direct = true
default = false
buildpack-id = "samples/dummy"

[[buildpacks]]
id = "samples/dummy"
api = "0.12"
"#;
    write_metadata(&layers, metadata);

    let mut cmd = base_cmd(&layers, &app);
    cmd.env("CNB_PLATFORM_API", "0.15");
    let output = cmd.output().expect("spawn launcher");

    assert_eq!(
        output.status.code(),
        Some(82),
        "expected exit 82; combined:\n{}",
        combined(&output)
    );
    let c = combined(&output);
    assert!(
        c.contains("when there is no default process a command is required"),
        "expected no-default message; got:\n{c}"
    );
}

// ===========================================================================
// 26. empty_command_entries_handled
//    (launch.rs:187 EmptyCommand: a process whose command array is empty errors
//     during resolution -> exit 82.)
// ===========================================================================
#[test]
fn empty_command_entries_handled() {
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    let metadata = r#"
[[processes]]
type = "web"
command = []
args = []
direct = true
buildpack-id = "samples/dummy"

[[buildpacks]]
id = "samples/dummy"
api = "0.10"
"#;
    write_metadata(&layers, metadata);

    let mut cmd = base_cmd(&layers, &app);
    cmd.arg0("web").env("CNB_PLATFORM_API", "0.15");
    let output = cmd.output().expect("spawn launcher");

    assert_eq!(
        output.status.code(),
        Some(82),
        "expected exit 82 for empty command entries; combined:\n{}",
        combined(&output)
    );
    let c = combined(&output);
    assert!(
        c.contains("Command entries list is empty") || c.contains("empty"),
        "expected empty-command error; got:\n{c}"
    );
}

// ===========================================================================
// 27. path_lookup_failure_exits_82
//    (launch.rs:261 which_in NotFound -> DirectExec error -> exit 82.)
// ===========================================================================
#[test]
fn path_lookup_failure_exits_82() {
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    let metadata = r#"
processes = []

[[buildpacks]]
id = "samples/dummy"
api = "0.10"
"#;
    write_metadata(&layers, metadata);

    let mut cmd = base_cmd(&layers, &app);
    cmd.env("CNB_PLATFORM_API", "0.15")
        .args(["--", "this-binary-most-definitely-does-not-exist-xyz"]);
    let output = cmd.output().expect("spawn launcher");

    assert_eq!(
        output.status.code(),
        Some(82),
        "expected exit 82 for PATH lookup failure; combined:\n{}",
        combined(&output)
    );
}

// ===========================================================================
// 28. deprecation_mode_excluded
//    (env.rs:45 CNB_DEPRECATION_MODE is in LAUNCH_ENV_EXCLUDELIST -> dropped
//     from the child env.)
// ===========================================================================
#[test]
fn deprecation_mode_excluded() {
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    let metadata = r#"
processes = []

[[buildpacks]]
id = "samples/dummy"
api = "0.12"
"#;
    write_metadata(&layers, metadata);

    let mut cmd = base_cmd(&layers, &app);
    cmd.env("CNB_PLATFORM_API", "0.12")
        .env("CNB_DEPRECATION_MODE", "warn")
        .args(["--", "/usr/bin/env"]);
    let output = cmd.output().expect("spawn launcher");

    assert!(
        output.status.success(),
        "launcher failed: stdout=\n{}\nstderr=\n{}",
        stdout_of(&output),
        stderr_of(&output)
    );
    let stdout = stdout_of(&output);
    assert!(
        !stdout.lines().any(|l| l.starts_with("CNB_DEPRECATION_MODE=")),
        "CNB_DEPRECATION_MODE must be excluded from child env; got:\n{stdout}"
    );
}

// ===========================================================================
// 29. cnb_process_type_warning_on_stdout_via_direct  [GAP]
//    (integration.rs:913 cnb_process_type_warning_contains_entrypoint_suggestion)
// ===========================================================================
// GAP: launcher-rs prints the CNB_PROCESS_TYPE entrypoint-suggestion warning to
// STDERR (main.rs print_warning -> eprintln), whereas the Go launcher's
// DefaultLogger writes it to STDOUT. Asserting the Go-correct STDOUT location
// FAILS against launcher-rs; the string is on stderr instead.
#[test]
fn cnb_process_type_warning_on_stdout_via_direct() {
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    let metadata = r#"
[[processes]]
type = "web"
command = "/bin/echo"
args = ["hello"]
direct = true
default = true

[[buildpacks]]
api = "0.9"
id = "test/bp"
"#;
    write_metadata(&layers, metadata);

    // No arg0 override -> argv0 basename is 'launcher', which gates the warning.
    let mut cmd = base_cmd(&layers, &app);
    cmd.env("CNB_PLATFORM_API", "0.12")
        .env("CNB_PROCESS_TYPE", "direct-process")
        .args(["--", "/bin/echo", "hello"]);
    let output = cmd.output().expect("spawn launcher");

    let stdout = stdout_of(&output);
    assert!(
        stdout.contains(
            "Warning: Run with ENTRYPOINT 'direct-process' to invoke the 'direct-process' process type"
        ),
        "expected entrypoint-suggestion warning on STDOUT (Go-correct); stdout:\n{stdout}\nstderr:\n{}",
        stderr_of(&output)
    );
}

// ===========================================================================
// 30. env_prepend_path_delim_gap  [GAP]
//    (integration.rs:502 env_dir_processing_with_fixtures, prepend WITHOUT
//     .delim — pins the Go PATH-like ':' default delimiter.)
// ===========================================================================
// GAP: launcher-rs uses an EMPTY default delimiter for Prepend on ALL names
// (env.rs:260 delim.unwrap_or("")), so a PATH.prepend with no .delim file
// yields "/extra/bin/usr/bin..." (raw concat). The Go semantics for PATH-like
// names prepend with os.PathListSeparator (":"), yielding "/extra/bin:...".
// Asserting the Go-correct ":" join FAILS.
#[test]
fn env_prepend_path_delim_gap() {
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    let metadata = r#"
processes = []

[[buildpacks]]
id = "samples/dummy"
api = "0.10"
"#;
    write_metadata(&layers, metadata);

    let layer = make_layer(&layers, "samples_dummy", "layer1");
    let env_dir = layer.join("env");
    fs::create_dir_all(&env_dir).unwrap();
    // NOTE: no PATH.delim file -> exposes the empty-default-delim divergence.
    fs::write(env_dir.join("PATH.prepend"), "/extra/bin").unwrap();

    let mut cmd = base_cmd(&layers, &app);
    cmd.env("CNB_PLATFORM_API", "0.12")
        .args(["--", "/usr/bin/env"]);
    let output = cmd.output().expect("spawn launcher");

    assert!(
        output.status.success(),
        "launcher failed: stdout=\n{}\nstderr=\n{}",
        stdout_of(&output),
        stderr_of(&output)
    );
    let stdout = stdout_of(&output);
    let path = path_line(&stdout).unwrap_or_else(|| panic!("no PATH= line; got:\n{stdout}"));
    // Go-correct: PATH-like prepend uses ':' default -> first entry is exactly "/extra/bin".
    assert_eq!(
        path.split(':').next(),
        Some("/extra/bin"),
        "Go-correct PATH-like prepend should ':'-join; PATH={path}"
    );
}

// ===========================================================================
// 31. golden_cnb_process_type_warning_symlink_argv0_on_stdout  [GAP]
//    (golden_test.rs:297 cnb_process_type_warning, ARGV0='web' symlink)
// ===========================================================================
// GAP (double divergence): (1) launcher-rs only emits the CNB_PROCESS_TYPE
// warnings when argv0 basename == "launcher" (main.rs gates p_type_opt on
// process_name=="launcher"); with argv0 symlinked to "web" it emits NOTHING.
// (2) Even when emitted, the warnings go to STDERR not STDOUT. The Go golden
// expected.stdout shows BOTH warning lines on STDOUT. Asserting that FAILS.
#[test]
fn golden_cnb_process_type_warning_symlink_argv0_on_stdout() {
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    let metadata = r#"
[[processes]]
type = "web"
command = ["/bin/echo"]
args = []
direct = true
buildpack-id = "samples/dummy"

[[buildpacks]]
id = "samples/dummy"
api = "0.10"
"#;
    write_metadata(&layers, metadata);

    let mut cmd = base_cmd(&layers, &app);
    cmd.arg0("web")
        .env("CNB_PLATFORM_API", "0.15")
        .env("CNB_PROCESS_TYPE", "web");
    let output = cmd.output().expect("spawn launcher");

    let stdout = stdout_of(&output);
    assert!(
        stdout.contains("Warning: CNB_PROCESS_TYPE is not supported in Platform API 0.15"),
        "expected unsupported-CNB_PROCESS_TYPE warning on STDOUT (Go-correct); stdout:\n{stdout}\nstderr:\n{}",
        stderr_of(&output)
    );
    assert!(
        stdout
            .contains("Warning: Run with ENTRYPOINT 'web' to invoke the 'web' process type"),
        "expected entrypoint-suggestion warning on STDOUT (Go-correct); stdout:\n{stdout}\nstderr:\n{}",
        stderr_of(&output)
    );
}
