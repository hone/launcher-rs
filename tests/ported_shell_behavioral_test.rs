//! Ported shell/bash behavioral tests.
//!
//! Mirrors launcher-rust's shell-focused tests from `src/launch/bash.rs`
//! (`BashShell::build_argv` / `bash_command_with_tokens`), `src/launch/shell.rs`
//! (`ShellProcess` / `collect_profiles` / `is_script`), and the shell-mode
//! integration cases in `tests/integration.rs`.
//!
//! launcher-rs is BINARY-ONLY (no [lib]); none of `BashShell`, `ShellProcess`,
//! `bash_command_with_tokens`, or `collect_profiles` are reachable from `tests/`.
//! Every launcher-rust unit assertion is therefore re-expressed BEHAVIORALLY:
//! we drive `target/debug/launcher` through its indirect (shell) path and observe
//! the bash-sourced/expanded result on stdout, exactly as a real consumer would.
//!
//! Dev-deps allow only `tempfile`. We use `std::process::Command` +
//! `std::os::unix::process::CommandExt::arg0()` and a canonicalized binary path,
//! matching the pattern in `tests/integration_test.rs`.

#![cfg(unix)]

use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::tempdir;

/// Canonicalized path to the compiled launcher binary.
/// Mirrors `tests/integration_test.rs` / `tests/exec_d_test.rs`.
fn launcher_bin() -> PathBuf {
    Path::new("target/debug/launcher")
        .canonicalize()
        .expect("launcher binary not found at target/debug/launcher; run `cargo build` first")
}

/// True iff `/bin/bash` exists (indirect shell mode requires it). Tests that
/// need bash skip gracefully on hosts without it, mirroring launcher-rust's
/// `if !Path::new("/bin/bash").exists() { ... return; }` guard.
fn have_bash() -> bool {
    Path::new("/bin/bash").exists()
}

/// Write `metadata.toml` under `<layers>/config/`.
fn write_metadata(layers: &Path, body: &str) {
    let config = layers.join("config");
    fs::create_dir_all(&config).unwrap();
    fs::write(config.join("metadata.toml"), body).unwrap();
}

/// Create `<layers>/<escaped-bp>/<layer>/` and return the layer dir path.
fn make_layer(layers: &Path, escaped_bp: &str, layer: &str) -> PathBuf {
    let p = layers.join(escaped_bp).join(layer);
    fs::create_dir_all(&p).unwrap();
    p
}

/// Symlink the launcher under `<tmp>/_link/<name>` so argv0 basename resolves
/// to a process type, returning the link path. (We also set `.arg0(name)` so
/// the selection works regardless of how the symlink target resolves.)
fn symlink_launcher(tmp: &Path, name: &str) -> PathBuf {
    let link_dir = tmp.join("_link");
    fs::create_dir_all(&link_dir).unwrap();
    let link = link_dir.join(name);
    std::os::unix::fs::symlink(launcher_bin(), &link).unwrap();
    link
}

/// Minimal metadata declaring a single buildpack `my-bp` (api 0.12) and one
/// `web` process. `command_toml` is the raw TOML value for `command`,
/// `args_toml` for `args`, `direct` selects the launch mode.
fn metadata_web(command_toml: &str, args_toml: &str, direct: bool) -> String {
    format!(
        r#"
[[processes]]
type = "web"
command = {command_toml}
args = {args_toml}
direct = {direct}
default = true
buildpack-id = "my-bp"

[[buildpacks]]
id = "my-bp"
api = "0.12"
"#
    )
}

// ---------------------------------------------------------------------------
// shell_no_args_runs
//
// Mirrors bash.rs `script_form_no_args` + shell.rs `is_script_empty_args`:
// when no args are supplied the process runs in script form
// (`exec bash -c "$@"`) and executes successfully. We observe via a shell-mode
// web process whose command writes a sentinel to stdout.
// ---------------------------------------------------------------------------
#[test]
fn shell_no_args_runs() {
    if !have_bash() {
        eprintln!("/bin/bash missing; skipping shell_no_args_runs");
        return;
    }
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    // Script form: command is a verbatim shell script, no args.
    write_metadata(&layers, &metadata_web(r#""echo script-form-ran""#, "[]", false));

    let link = symlink_launcher(tmp.path(), "web");
    let output = Command::new(&link)
        .arg0("web")
        .env("CNB_PLATFORM_API", "0.15")
        .env("CNB_LAYERS_DIR", &layers)
        .env("CNB_APP_DIR", &app)
        .output()
        .expect("spawn launcher");

    assert!(
        output.status.success(),
        "launcher failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("script-form-ran"),
        "expected script-form output; got: {stdout}"
    );
}

// ---------------------------------------------------------------------------
// shell_with_args_expands
//
// Mirrors bash.rs `one_arg_eval_echo_wrapped`: a non-script shell process with
// one arg builds the `eval echo`-wrapped token form and runs it. The arg is
// passed through to the command and printed. Go-correct: the user arg reaches
// the command verbatim.
// ---------------------------------------------------------------------------
#[test]
fn shell_with_args_expands() {
    if !have_bash() {
        eprintln!("/bin/bash missing; skipping shell_with_args_expands");
        return;
    }
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    // Non-script form: command + one arg => nTokens = 2 (eval echo wrapping).
    write_metadata(&layers, &metadata_web(r#""echo""#, r#"["expanded-arg"]"#, false));

    let link = symlink_launcher(tmp.path(), "web");
    let output = Command::new(&link)
        .arg0("web")
        .env("CNB_PLATFORM_API", "0.15")
        .env("CNB_LAYERS_DIR", &layers)
        .env("CNB_APP_DIR", &app)
        .output()
        .expect("spawn launcher");

    assert!(
        output.status.success(),
        "launcher failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim_end(), "expanded-arg");
}

// ---------------------------------------------------------------------------
// indirect_shell_profiles_expand_dollar_args
//
// Mirrors integration.rs `test_integration_indirect_shell_profile_sourcing`
// (launcher-rs) and the Go-faithful $-expansion semantics from bash.rs: in
// indirect shell mode, args containing `$VAR` are expanded against values set
// by sourced profiles (buildpack profile.d + app .profile). Go-correct: the
// printed line is the expanded "sourced-bp-val sourced-app-val".
// ---------------------------------------------------------------------------
#[test]
fn indirect_shell_profiles_expand_dollar_args() {
    if !have_bash() {
        eprintln!("/bin/bash missing; skipping indirect_shell_profiles_expand_dollar_args");
        return;
    }
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    write_metadata(
        &layers,
        &metadata_web(r#""echo""#, r#"["$BP_ENV", "$APP_ENV"]"#, false),
    );

    let layer = make_layer(&layers, "my-bp", "test-layer");
    let profile_d = layer.join("profile.d");
    fs::create_dir_all(&profile_d).unwrap();
    fs::write(profile_d.join("01-bp.sh"), "export BP_ENV=\"sourced-bp-val\"\n").unwrap();
    fs::write(app.join(".profile"), "export APP_ENV=\"sourced-app-val\"\n").unwrap();

    let link = symlink_launcher(tmp.path(), "web");
    let output = Command::new(&link)
        .arg0("web")
        .env("CNB_PLATFORM_API", "0.15")
        .env("CNB_LAYERS_DIR", &layers)
        .env("CNB_APP_DIR", &app)
        .env("CNB_EXEC_ENV", "production")
        .output()
        .expect("spawn launcher");

    assert!(
        output.status.success(),
        "launcher failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("sourced-bp-val sourced-app-val"),
        "expected expanded profile values; got: {stdout}"
    );
}

// ---------------------------------------------------------------------------
// bash_token_eval_echo_roundtrip
//
// Mirrors bash.rs `bash_command_with_tokens` round-trip semantics behaviorally:
// the `eval echo` token wrapper performs shell expansion on each arg. A literal
// arg `$ROUNDTRIP` is expanded by the inner `eval echo` to the value carried in
// the launch env. Go-correct: the expanded value appears (not the literal
// "$ROUNDTRIP").
// ---------------------------------------------------------------------------
#[test]
fn bash_token_eval_echo_roundtrip() {
    if !have_bash() {
        eprintln!("/bin/bash missing; skipping bash_token_eval_echo_roundtrip");
        return;
    }
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    // command=echo, arg="$ROUNDTRIP" => eval echo expands $ROUNDTRIP.
    write_metadata(&layers, &metadata_web(r#""echo""#, r#"["$ROUNDTRIP"]"#, false));

    let link = symlink_launcher(tmp.path(), "web");
    let output = Command::new(&link)
        .arg0("web")
        // ROUNDTRIP is carried into the launch env (not on excludelist), so the
        // shell's `eval echo` expands $ROUNDTRIP to its value.
        .env("ROUNDTRIP", "eval-echo-worked")
        .env("CNB_PLATFORM_API", "0.15")
        .env("CNB_LAYERS_DIR", &layers)
        .env("CNB_APP_DIR", &app)
        .output()
        .expect("spawn launcher");

    assert!(
        output.status.success(),
        "launcher failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim_end(), "eval-echo-worked");
}

// ---------------------------------------------------------------------------
// profile_d_and_dotprofile_order
//
// Mirrors shell.rs `collect_profiles_appends_app_profile_last`: buildpack
// profile.d scripts are sourced first, then the app `.profile` LAST. We prove
// ordering by having profile.d set ORDER then .profile OVERRIDE it; the LAST
// writer (.profile) wins. Go-correct: ORDER=from_dotprofile.
// ---------------------------------------------------------------------------
#[test]
fn profile_d_and_dotprofile_order() {
    if !have_bash() {
        eprintln!("/bin/bash missing; skipping profile_d_and_dotprofile_order");
        return;
    }
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    write_metadata(&layers, &metadata_web(r#""echo""#, r#"["$ORDER"]"#, false));

    let layer = make_layer(&layers, "my-bp", "test-layer");
    let profile_d = layer.join("profile.d");
    fs::create_dir_all(&profile_d).unwrap();
    fs::write(profile_d.join("00-setup.sh"), "export ORDER=from_profiled\n").unwrap();
    fs::write(app.join(".profile"), "export ORDER=from_dotprofile\n").unwrap();

    let link = symlink_launcher(tmp.path(), "web");
    let output = Command::new(&link)
        .arg0("web")
        .env("CNB_PLATFORM_API", "0.15")
        .env("CNB_LAYERS_DIR", &layers)
        .env("CNB_APP_DIR", &app)
        .output()
        .expect("spawn launcher");

    assert!(
        output.status.success(),
        "launcher failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim_end(),
        "from_dotprofile",
        "app .profile must source LAST and win over profile.d"
    );
}

// ---------------------------------------------------------------------------
// app_profile_sourced
//
// Mirrors integration.rs (app `.profile` path) + shell.rs
// `collect_profiles_appends_app_profile_last`: the app `<app>/.profile` is
// sourced in indirect mode and its exported var reaches the process. Go-correct:
// APP_ONLY=from_app_profile in output.
// ---------------------------------------------------------------------------
#[test]
fn app_profile_sourced() {
    if !have_bash() {
        eprintln!("/bin/bash missing; skipping app_profile_sourced");
        return;
    }
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    write_metadata(&layers, &metadata_web(r#""echo""#, r#"["$APP_ONLY"]"#, false));
    fs::write(app.join(".profile"), "export APP_ONLY=from_app_profile\n").unwrap();

    let link = symlink_launcher(tmp.path(), "web");
    let output = Command::new(&link)
        .arg0("web")
        .env("CNB_PLATFORM_API", "0.15")
        .env("CNB_LAYERS_DIR", &layers)
        .env("CNB_APP_DIR", &app)
        .output()
        .expect("spawn launcher");

    assert!(
        output.status.success(),
        "launcher failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim_end(), "from_app_profile");
}

// ---------------------------------------------------------------------------
// bp_profile_sourced
//
// Mirrors integration.rs / shell.rs profile.d sourcing: a buildpack-contributed
// `profile.d/*.sh` is sourced in indirect mode and its exported var reaches the
// process. Go-correct: BP_ONLY=from_bp_profile in output.
// ---------------------------------------------------------------------------
#[test]
fn bp_profile_sourced() {
    if !have_bash() {
        eprintln!("/bin/bash missing; skipping bp_profile_sourced");
        return;
    }
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    write_metadata(&layers, &metadata_web(r#""echo""#, r#"["$BP_ONLY"]"#, false));

    let layer = make_layer(&layers, "my-bp", "test-layer");
    let profile_d = layer.join("profile.d");
    fs::create_dir_all(&profile_d).unwrap();
    fs::write(profile_d.join("01-bp.sh"), "export BP_ONLY=from_bp_profile\n").unwrap();

    let link = symlink_launcher(tmp.path(), "web");
    let output = Command::new(&link)
        .arg0("web")
        .env("CNB_PLATFORM_API", "0.15")
        .env("CNB_LAYERS_DIR", &layers)
        .env("CNB_APP_DIR", &app)
        .output()
        .expect("spawn launcher");

    assert!(
        output.status.success(),
        "launcher failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim_end(), "from_bp_profile");
}

// ---------------------------------------------------------------------------
// shell_process_no_default_args
//
// Mirrors shell.rs `is_script` dispatch + main.rs `script: args.is_empty()`:
// a shell process with NO args runs in script form (`exec bash -c "$@"`),
// where the command string is the entire script and there are zero positional
// parameters ($# == 0). Go-correct: script body runs with no extra args.
// ---------------------------------------------------------------------------
#[test]
fn shell_process_no_default_args() {
    if !have_bash() {
        eprintln!("/bin/bash missing; skipping shell_process_no_default_args");
        return;
    }
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    // Script form (args=[]) => command is the whole script; print arg count.
    write_metadata(
        &layers,
        &metadata_web(r#""echo no-args-count=$#""#, "[]", false),
    );

    let link = symlink_launcher(tmp.path(), "web");
    let output = Command::new(&link)
        .arg0("web")
        .env("CNB_PLATFORM_API", "0.15")
        .env("CNB_LAYERS_DIR", &layers)
        .env("CNB_APP_DIR", &app)
        .output()
        .expect("spawn launcher");

    assert!(
        output.status.success(),
        "launcher failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim_end(), "no-args-count=0");
}

// ---------------------------------------------------------------------------
// shell_multiple_args_quoted
//
// Mirrors bash.rs `two_args_extends_to_token_2`: a non-script shell process
// with 2 args builds the token wrapper extended to token 2, and both args are
// passed through as distinct positional tokens. Go-correct: both args appear,
// space-joined by echo, in order.
// ---------------------------------------------------------------------------
#[test]
fn shell_multiple_args_quoted() {
    if !have_bash() {
        eprintln!("/bin/bash missing; skipping shell_multiple_args_quoted");
        return;
    }
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    write_metadata(&layers, &metadata_web(r#""echo""#, r#"["alpha", "beta"]"#, false));

    let link = symlink_launcher(tmp.path(), "web");
    let output = Command::new(&link)
        .arg0("web")
        .env("CNB_PLATFORM_API", "0.15")
        .env("CNB_LAYERS_DIR", &layers)
        .env("CNB_APP_DIR", &app)
        .output()
        .expect("spawn launcher");

    assert!(
        output.status.success(),
        "launcher failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim_end(), "alpha beta");
}

// ---------------------------------------------------------------------------
// shell_env_var_in_arg_expands
//
// Mirrors the $-expansion of bash.rs `eval echo` wrapping for a single env var
// embedded in an arg: an arg `prefix-$MYVAR` carried in the launch env expands
// to `prefix-<value>` in the shell. Go-correct: the value, not literal "$MYVAR".
// ---------------------------------------------------------------------------
#[test]
fn shell_env_var_in_arg_expands() {
    if !have_bash() {
        eprintln!("/bin/bash missing; skipping shell_env_var_in_arg_expands");
        return;
    }
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    write_metadata(&layers, &metadata_web(r#""echo""#, r#"["prefix-$MYVAR"]"#, false));

    let link = symlink_launcher(tmp.path(), "web");
    let output = Command::new(&link)
        .arg0("web")
        .env("MYVAR", "injected")
        .env("CNB_PLATFORM_API", "0.15")
        .env("CNB_LAYERS_DIR", &layers)
        .env("CNB_APP_DIR", &app)
        .output()
        .expect("spawn launcher");

    assert!(
        output.status.success(),
        "launcher failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim_end(), "prefix-injected");
}

// ---------------------------------------------------------------------------
// shell_exit_code_propagates
//
// Mirrors the shell launch contract (main.rs indirect path execs bash, which
// execs the command, so the command's exit code is the launcher's). A shell
// process exiting non-zero propagates that exact code. Go-correct: exit 7.
// ---------------------------------------------------------------------------
#[test]
fn shell_exit_code_propagates() {
    if !have_bash() {
        eprintln!("/bin/bash missing; skipping shell_exit_code_propagates");
        return;
    }
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    // Script form: `exit 7` is the verbatim shell script.
    write_metadata(&layers, &metadata_web(r#""exit 7""#, "[]", false));

    let link = symlink_launcher(tmp.path(), "web");
    let output = Command::new(&link)
        .arg0("web")
        .env("CNB_PLATFORM_API", "0.15")
        .env("CNB_LAYERS_DIR", &layers)
        .env("CNB_APP_DIR", &app)
        .output()
        .expect("spawn launcher");

    assert_eq!(
        output.status.code(),
        Some(7),
        "shell exit code must propagate; stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

// ---------------------------------------------------------------------------
// shell_special_chars_in_args
//
// Mirrors the quoting guarantees behind bash.rs token wrapping: an arg
// containing a shell metacharacter (`;`) plus an embedded space is preserved as
// a SINGLE literal token (the wrapper double-quotes each positional, so the `;`
// does not split commands and the space does not split the word). Go-correct:
// the arg arrives as one literal token "a;b c".
// ---------------------------------------------------------------------------
#[test]
fn shell_special_chars_in_args() {
    if !have_bash() {
        eprintln!("/bin/bash missing; skipping shell_special_chars_in_args");
        return;
    }
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    // printf "[%s]" with one arg wraps exactly that single argument in brackets;
    // if the ';' split commands or the space split the token, output differs.
    write_metadata(
        &layers,
        &metadata_web(r#""printf""#, r#"["[%s]", "a;b c"]"#, false),
    );

    let link = symlink_launcher(tmp.path(), "web");
    let output = Command::new(&link)
        .arg0("web")
        .env("CNB_PLATFORM_API", "0.15")
        .env("CNB_LAYERS_DIR", &layers)
        .env("CNB_APP_DIR", &app)
        .output()
        .expect("spawn launcher");

    assert!(
        output.status.success(),
        "launcher failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim_end(), "[a;b c]");
}

// ---------------------------------------------------------------------------
// shell_empty_args_ok
//
// Mirrors shell.rs `is_script_empty_args`/`script_form_no_args`: empty args is
// valid and produces a working script-form launch (no panic, success). Distinct
// from shell_no_args_runs by exercising a compound script body. Go-correct:
// success, sentinel printed.
// ---------------------------------------------------------------------------
#[test]
fn shell_empty_args_ok() {
    if !have_bash() {
        eprintln!("/bin/bash missing; skipping shell_empty_args_ok");
        return;
    }
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    write_metadata(
        &layers,
        &metadata_web(r#""true && echo empty-args-ok""#, "[]", false),
    );

    let link = symlink_launcher(tmp.path(), "web");
    let output = Command::new(&link)
        .arg0("web")
        .env("CNB_PLATFORM_API", "0.15")
        .env("CNB_LAYERS_DIR", &layers)
        .env("CNB_APP_DIR", &app)
        .output()
        .expect("spawn launcher");

    assert!(
        output.status.success(),
        "launcher failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("empty-args-ok"),
        "expected empty-args sentinel; got: {stdout}"
    );
}

// ---------------------------------------------------------------------------
// shell_profile_sets_var_seen_by_proc
//
// Mirrors integration.rs profile-sourcing: a var EXPORTED by a sourced profile
// is visible in the launched process's environment (not just expanded inline).
// We launch `env` (shell-mode) and grep the exported var line. Go-correct:
// PROFILE_SET=seen appears as an env line.
// ---------------------------------------------------------------------------
#[test]
fn shell_profile_sets_var_seen_by_proc() {
    if !have_bash() {
        eprintln!("/bin/bash missing; skipping shell_profile_sets_var_seen_by_proc");
        return;
    }
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    // direct=false, command=env (script form prints whole environment).
    write_metadata(&layers, &metadata_web(r#""env""#, "[]", false));

    let layer = make_layer(&layers, "my-bp", "test-layer");
    let profile_d = layer.join("profile.d");
    fs::create_dir_all(&profile_d).unwrap();
    fs::write(profile_d.join("01-set.sh"), "export PROFILE_SET=seen\n").unwrap();

    let link = symlink_launcher(tmp.path(), "web");
    let output = Command::new(&link)
        .arg0("web")
        .env("CNB_PLATFORM_API", "0.15")
        .env("CNB_LAYERS_DIR", &layers)
        .env("CNB_APP_DIR", &app)
        .output()
        .expect("spawn launcher");

    assert!(
        output.status.success(),
        "launcher failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.lines().any(|l| l == "PROFILE_SET=seen"),
        "expected PROFILE_SET=seen env line; got: {stdout}"
    );
}

// ---------------------------------------------------------------------------
// shell_sources_in_correct_cwd
//
// Mirrors bash.rs `cd_after_sources_before_exec` (the launcher `cd`s into the
// working directory before exec): the shell-launched process runs with PWD ==
// the app dir. The launcher uses the CNB_APP_DIR value verbatim as the cd
// target (no canonicalization), so bash's logical `pwd` echoes it back.
// Go-correct: pwd equals the supplied app dir.
// ---------------------------------------------------------------------------
#[test]
fn shell_sources_in_correct_cwd() {
    if !have_bash() {
        eprintln!("/bin/bash missing; skipping shell_sources_in_correct_cwd");
        return;
    }
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    // Script form prints the shell's working directory.
    write_metadata(&layers, &metadata_web(r#""pwd""#, "[]", false));

    let link = symlink_launcher(tmp.path(), "web");
    let output = Command::new(&link)
        .arg0("web")
        .env("CNB_PLATFORM_API", "0.15")
        .env("CNB_LAYERS_DIR", &layers)
        .env("CNB_APP_DIR", &app)
        .output()
        .expect("spawn launcher");

    assert!(
        output.status.success(),
        "launcher failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // The launcher `cd`s into working_directory == CNB_APP_DIR verbatim; bash's
    // logical `pwd` echoes that path back (no symlink canonicalization).
    assert_eq!(
        stdout.trim_end(),
        app.to_string_lossy(),
        "shell must cd into the app dir before exec"
    );
}

// ---------------------------------------------------------------------------
// shell_bp_then_app_profile_precedence
//
// Mirrors shell.rs `collect_profiles_appends_app_profile_last` precedence: when
// BOTH a buildpack profile.d AND the app .profile set the SAME var, the app
// .profile (sourced last) wins. Go-correct: app value wins.
// ---------------------------------------------------------------------------
#[test]
fn shell_bp_then_app_profile_precedence() {
    if !have_bash() {
        eprintln!("/bin/bash missing; skipping shell_bp_then_app_profile_precedence");
        return;
    }
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    write_metadata(&layers, &metadata_web(r#""echo""#, r#"["$SHARED"]"#, false));

    let layer = make_layer(&layers, "my-bp", "test-layer");
    let profile_d = layer.join("profile.d");
    fs::create_dir_all(&profile_d).unwrap();
    fs::write(profile_d.join("01-bp.sh"), "export SHARED=bp-wins\n").unwrap();
    fs::write(app.join(".profile"), "export SHARED=app-wins\n").unwrap();

    let link = symlink_launcher(tmp.path(), "web");
    let output = Command::new(&link)
        .arg0("web")
        .env("CNB_PLATFORM_API", "0.15")
        .env("CNB_LAYERS_DIR", &layers)
        .env("CNB_APP_DIR", &app)
        .output()
        .expect("spawn launcher");

    assert!(
        output.status.success(),
        "launcher failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim_end(), "app-wins");
}

// ---------------------------------------------------------------------------
// shell_token_count_matches_args
//
// Mirrors bash.rs `tokens_n*` / `one_arg_eval_echo_wrapped` /
// `two_args_extends_to_token_2`: the token wrapper produces exactly one
// positional slot per arg, so N args survive as N distinct positionals. We
// verify N=3 with `printf "[%s]"`, which brackets each remaining arg
// individually. Go-correct: "[one][two][three]".
// ---------------------------------------------------------------------------
#[test]
fn shell_token_count_matches_args() {
    if !have_bash() {
        eprintln!("/bin/bash missing; skipping shell_token_count_matches_args");
        return;
    }
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    // Non-script: command=printf, format "[%s]" reused for each remaining arg.
    write_metadata(
        &layers,
        &metadata_web(r#""printf""#, r#"["[%s]", "one", "two", "three"]"#, false),
    );

    let link = symlink_launcher(tmp.path(), "web");
    let output = Command::new(&link)
        .arg0("web")
        .env("CNB_PLATFORM_API", "0.15")
        .env("CNB_LAYERS_DIR", &layers)
        .env("CNB_APP_DIR", &app)
        .output()
        .expect("spawn launcher");

    assert!(
        output.status.success(),
        "launcher failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim_end(),
        "[one][two][three]",
        "each arg must survive as a distinct positional token"
    );
}

// ---------------------------------------------------------------------------
// shell_direct_false_uses_bash
//
// Mirrors shell.rs/bash.rs dispatch + main.rs indirect branch: when
// direct=false the launcher routes through /bin/bash. We confirm a bash-only
// construct (the BASH_VERSION builtin var) is populated, which proves bash (not
// direct exec) ran the command. Go-correct: BASH_VERSION is non-empty.
// ---------------------------------------------------------------------------
#[test]
fn shell_direct_false_uses_bash() {
    if !have_bash() {
        eprintln!("/bin/bash missing; skipping shell_direct_false_uses_bash");
        return;
    }
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    // Script form: print BASH_VERSION; only bash sets this var.
    write_metadata(
        &layers,
        &metadata_web(r#""echo bash-version=${BASH_VERSION:-none}""#, "[]", false),
    );

    let link = symlink_launcher(tmp.path(), "web");
    let output = Command::new(&link)
        .arg0("web")
        .env("CNB_PLATFORM_API", "0.15")
        .env("CNB_LAYERS_DIR", &layers)
        .env("CNB_APP_DIR", &app)
        .output()
        .expect("spawn launcher");

    assert!(
        output.status.success(),
        "launcher failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("bash-version=") && !stdout.contains("bash-version=none"),
        "indirect mode must run via bash (BASH_VERSION set); got: {stdout}"
    );
}

// ---------------------------------------------------------------------------
// shell_quotes_preserved
//
// Mirrors the word-boundary guarantee of bash.rs token wrapping: each token is
// emitted as `"$(eval echo \"${N}\")"`, whose OUTER double quotes keep an arg
// with embedded whitespace as a SINGLE word. So an arg `keep together` is not
// split into two tokens. Go-correct: the arg arrives as one bracketed token
// "[keep together]". (Note: `eval echo` strips the user's own quote characters,
// so we assert preserved word grouping, the property the wrapper guarantees.)
// ---------------------------------------------------------------------------
#[test]
fn shell_quotes_preserved() {
    if !have_bash() {
        eprintln!("/bin/bash missing; skipping shell_quotes_preserved");
        return;
    }
    let tmp = tempdir().unwrap();
    let layers = tmp.path().join("_layers");
    let app = tmp.path().join("_app");
    fs::create_dir_all(&app).unwrap();
    write_metadata(
        &layers,
        &metadata_web(r#""printf""#, r#"["[%s]", "keep together"]"#, false),
    );

    let link = symlink_launcher(tmp.path(), "web");
    let output = Command::new(&link)
        .arg0("web")
        .env("CNB_PLATFORM_API", "0.15")
        .env("CNB_LAYERS_DIR", &layers)
        .env("CNB_APP_DIR", &app)
        .output()
        .expect("spawn launcher");

    assert!(
        output.status.success(),
        "launcher failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim_end(),
        "[keep together]",
        "the token wrapper must keep a whitespace-bearing arg as one word"
    );
}
