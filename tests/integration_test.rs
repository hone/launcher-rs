#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use tempfile::tempdir;

#[test]
fn test_integration_launcher_e2e() {
    // 1. Setup mock layers and app sandbox directories
    let layers_temp = tempdir().unwrap();
    let layers_path = layers_temp.path();
    let app_temp = tempdir().unwrap();
    let app_path = app_temp.path();

    // Create config directory and metadata.toml
    let config_dir = layers_path.join("config");
    fs::create_dir_all(&config_dir).unwrap();

    let metadata_content = r#"
[[processes]]
type = "web"
command = ["echo"]
args = ["hello-from-direct-web"]
direct = true
default = true
buildpack-id = "my-bp"

[[buildpacks]]
id = "my-bp"
api = "0.12"
"#;
    fs::write(config_dir.join("metadata.toml"), metadata_content).unwrap();

    // Create buildpack layer directory structure
    let layer_dir = layers_path.join("my-bp").join("test-layer");
    fs::create_dir_all(&layer_dir).unwrap();

    // 1. Env directory overrides
    let env_dir = layer_dir.join("env");
    fs::create_dir_all(&env_dir).unwrap();
    fs::write(env_dir.join("FOO"), "bar").unwrap();

    let env_launch_dir = layer_dir.join("env.launch");
    fs::create_dir_all(&env_launch_dir).unwrap();
    fs::write(env_launch_dir.join("VAR.append"), "suffix").unwrap();
    fs::write(env_launch_dir.join("VAR.delim"), "-").unwrap();

    // 2. Exec.d scripts
    let exec_d_dir = layer_dir.join("exec.d");
    fs::create_dir_all(&exec_d_dir).unwrap();
    let exec_d_script = exec_d_dir.join("01-inject.sh");

    let script_content = r#"#!/bin/bash
echo 'INJECTED_VAR = "injected_exec_d"' >&3
echo "EXEC_D_PWD = \"$(pwd)\"" >&3
"#;
    fs::write(&exec_d_script, script_content).unwrap();

    // Make the exec.d script executable
    let mut perms = fs::metadata(&exec_d_script).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&exec_d_script, perms).unwrap();

    // 3. Locate compiled launcher binary
    let launcher_bin = Path::new("target/debug/launcher").canonicalize();
    assert!(
        launcher_bin.is_ok(),
        "Launcher binary not found at target/debug/launcher. Please run cargo build first."
    );
    let launcher_bin = launcher_bin.unwrap();

    // 4. Spawn the launcher
    let mut cmd = Command::new(&launcher_bin);
    cmd.env("CNB_PLATFORM_API", "0.15");
    cmd.env("CNB_LAYERS_DIR", layers_path.to_string_lossy().to_string());
    cmd.env("CNB_APP_DIR", app_path.to_string_lossy().to_string());
    cmd.env("CNB_EXEC_ENV", "production");
    cmd.env("VAR", "base");

    // We pass custom command-line overrides to launch standard "env" program
    cmd.arg("--").arg("env");

    let output = cmd.output().unwrap();

    assert!(
        output.status.success(),
        "Launcher failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout_str = String::from_utf8_lossy(&output.stdout);

    // 5. Assertions on final environment!
    assert!(
        stdout_str.contains("FOO=bar"),
        "Missing FOO=bar in final env. Stdout: {}",
        stdout_str
    );
    assert!(
        stdout_str.contains("VAR=base-suffix"),
        "Missing VAR=base-suffix in final env. Stdout: {}",
        stdout_str
    );
    assert!(
        stdout_str.contains("INJECTED_VAR=injected_exec_d"),
        "Missing INJECTED_VAR in final env. Stdout: {}",
        stdout_str
    );
    let canonical_app_path = app_path.canonicalize().unwrap();
    let expected_pwd_env = format!("EXEC_D_PWD={}", canonical_app_path.to_string_lossy());
    assert!(
        stdout_str.contains(&expected_pwd_env),
        "Missing or incorrect EXEC_D_PWD. Expected: {}, Stdout: {}",
        expected_pwd_env,
        stdout_str
    );
}

#[test]
fn test_integration_platform_api_incompatible() {
    let layers_temp = tempdir().unwrap();
    let launcher_bin = Path::new("target/debug/launcher").canonicalize().unwrap();

    let mut cmd = Command::new(&launcher_bin);
    cmd.env("CNB_PLATFORM_API", "0.5"); // Unsupported API version
    cmd.env(
        "CNB_LAYERS_DIR",
        layers_temp.path().to_string_lossy().to_string(),
    );

    let output = cmd.output().unwrap();
    assert_eq!(
        output.status.code().unwrap(),
        11,
        "Expected exit code 11 for platform incompatibility"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("incompatible with the lifecycle"),
        "Unexpected error: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_integration_buildpack_api_incompatible() {
    let layers_temp = tempdir().unwrap();
    let layers_path = layers_temp.path();
    let launcher_bin = Path::new("target/debug/launcher").canonicalize().unwrap();

    let config_dir = layers_path.join("config");
    fs::create_dir_all(&config_dir).unwrap();

    let metadata_content = r#"
[[processes]]
type = "web"
command = ["echo"]
args = ["test"]
direct = true
default = true
buildpack-id = "bad-bp"

[[buildpacks]]
id = "bad-bp"
api = "0.5" # Unsupported Buildpack API version
"#;
    fs::write(config_dir.join("metadata.toml"), metadata_content).unwrap();

    let mut cmd = Command::new(&launcher_bin);
    cmd.env("CNB_PLATFORM_API", "0.15");
    cmd.env("CNB_LAYERS_DIR", layers_path.to_string_lossy().to_string());

    let output = cmd.output().unwrap();
    assert_eq!(
        output.status.code().unwrap(),
        12,
        "Expected exit code 12 for buildpack incompatibility"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("incompatible with the lifecycle"),
        "Unexpected error: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_integration_process_not_found() {
    let layers_temp = tempdir().unwrap();
    let layers_path = layers_temp.path();
    let launcher_bin = Path::new("target/debug/launcher").canonicalize().unwrap();

    let config_dir = layers_path.join("config");
    fs::create_dir_all(&config_dir).unwrap();

    let metadata_content = r#"
[[processes]]
type = "web"
command = ["echo"]
args = ["test"]
direct = true
default = false # No default process
buildpack-id = "my-bp"

[[buildpacks]]
id = "my-bp"
api = "0.12"
"#;
    fs::write(config_dir.join("metadata.toml"), metadata_content).unwrap();

    let mut cmd = Command::new(&launcher_bin);
    cmd.env("CNB_PLATFORM_API", "0.15");
    cmd.env("CNB_LAYERS_DIR", layers_path.to_string_lossy().to_string());

    let output = cmd.output().unwrap();
    assert_eq!(
        output.status.code().unwrap(),
        82,
        "Expected exit code 82 for generic launch error"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("when there is no default process a command is required"),
        "Unexpected error: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_integration_indirect_shell_profile_sourcing() {
    let layers_temp = tempdir().unwrap();
    let layers_path = layers_temp.path();
    let app_temp = tempdir().unwrap();
    let app_path = app_temp.path();
    let launcher_bin = Path::new("target/debug/launcher").canonicalize().unwrap();

    let config_dir = layers_path.join("config");
    fs::create_dir_all(&config_dir).unwrap();

    let metadata_content = r#"
[[processes]]
type = "worker"
command = ["echo"]
args = ["$BP_ENV", "$APP_ENV"]
direct = false
default = true
buildpack-id = "my-bp"

[[buildpacks]]
id = "my-bp"
api = "0.12"
"#;
    fs::write(config_dir.join("metadata.toml"), metadata_content).unwrap();

    let layer_dir = layers_path.join("my-bp").join("test-layer");
    let profile_d_dir = layer_dir.join("profile.d");
    fs::create_dir_all(&profile_d_dir).unwrap();

    // Write a buildpack profile script
    let bp_profile = profile_d_dir.join("01-bp.sh");
    fs::write(&bp_profile, "export BP_ENV=\"sourced-bp-val\"\n").unwrap();

    // Write an app profile script
    let app_profile = app_path.join(".profile");
    fs::write(&app_profile, "export APP_ENV=\"sourced-app-val\"\n").unwrap();

    let mut cmd = Command::new(&launcher_bin);
    cmd.env("CNB_PLATFORM_API", "0.15");
    cmd.env("CNB_LAYERS_DIR", layers_path.to_string_lossy().to_string());
    cmd.env("CNB_APP_DIR", app_path.to_string_lossy().to_string());
    cmd.env("CNB_EXEC_ENV", "production");

    let output = cmd.output().unwrap();
    assert!(
        output.status.success(),
        "Launcher failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout_str = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout_str.contains("sourced-bp-val sourced-app-val"),
        "Missing sourced env values from profiles! Stdout: {}",
        stdout_str
    );
}

#[test]
fn test_integration_platform_api_unset() {
    let layers_temp = tempdir().unwrap();
    let launcher_bin = Path::new("target/debug/launcher").canonicalize().unwrap();

    let mut cmd = Command::new(&launcher_bin);
    // Explicitly remove CNB_PLATFORM_API to test the unset default behavior
    cmd.env_remove("CNB_PLATFORM_API");
    cmd.env(
        "CNB_LAYERS_DIR",
        layers_temp.path().to_string_lossy().to_string(),
    );

    let output = cmd.output().unwrap();
    assert_eq!(
        output.status.code().unwrap(),
        11,
        "Expected exit code 11 for unset platform API"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("failed to get platform API version; please set 'CNB_PLATFORM_API' to specify the desired platform API version"),
        "Unexpected error output: {}", stderr
    );
}

#[test]
fn test_integration_platform_api_empty() {
    let layers_temp = tempdir().unwrap();
    let launcher_bin = Path::new("target/debug/launcher").canonicalize().unwrap();

    let mut cmd = Command::new(&launcher_bin);
    cmd.env("CNB_PLATFORM_API", "");
    cmd.env(
        "CNB_LAYERS_DIR",
        layers_temp.path().to_string_lossy().to_string(),
    );

    let output = cmd.output().unwrap();
    assert_eq!(
        output.status.code().unwrap(),
        11,
        "Expected exit code 11 for empty platform API"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("failed to get platform API version; please set 'CNB_PLATFORM_API' to specify the desired platform API version"),
        "Unexpected error output: {}", stderr
    );
}

#[test]
fn test_integration_platform_api_invalid() {
    let layers_temp = tempdir().unwrap();
    let launcher_bin = Path::new("target/debug/launcher").canonicalize().unwrap();

    let mut cmd = Command::new(&launcher_bin);
    cmd.env("CNB_PLATFORM_API", "bad-api-version");
    cmd.env(
        "CNB_LAYERS_DIR",
        layers_temp.path().to_string_lossy().to_string(),
    );

    let output = cmd.output().unwrap();
    assert_eq!(
        output.status.code().unwrap(),
        11,
        "Expected exit code 11 for invalid platform API"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("failed to parse platform API 'bad-api-version'"),
        "Unexpected error output: {}",
        stderr
    );
}

#[test]
fn test_integration_cnb_process_type_warn() {
    let layers_temp = tempdir().unwrap();
    let layers_path = layers_temp.path();
    let launcher_bin = Path::new("target/debug/launcher").canonicalize().unwrap();

    let config_dir = layers_path.join("config");
    fs::create_dir_all(&config_dir).unwrap();

    let metadata_content = r#"
[[processes]]
type = "web"
command = ["echo"]
args = ["test"]
direct = true
default = true
buildpack-id = "my-bp"

[[buildpacks]]
id = "my-bp"
api = "0.12"
"#;
    fs::write(config_dir.join("metadata.toml"), metadata_content).unwrap();

    let mut cmd = Command::new(&launcher_bin);
    cmd.env("CNB_PLATFORM_API", "0.15");
    cmd.env("CNB_LAYERS_DIR", layers_path.to_string_lossy().to_string());
    cmd.env("CNB_PROCESS_TYPE", "direct-process");

    let output = cmd.output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("Warning: CNB_PROCESS_TYPE is not supported in Platform API 0.15"),
        "Expected warning message not found in stderr: {}",
        stderr
    );
    assert!(
        stderr.contains("Warning: Run with ENTRYPOINT 'direct-process' to invoke the 'direct-process' process type"),
        "Expected second warning message not found in stderr: {}",
        stderr
    );
}
