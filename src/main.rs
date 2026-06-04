pub mod api;
pub mod env;
pub mod exec_d;
pub mod exit;
pub mod launch;
pub mod shell;

use env::{ActionType, LaunchEnv};
use exit::ExitCode;
use launch::RawMetadata;
#[cfg(unix)]
use shell::BashShell;
#[cfg(windows)]
use shell::CmdShell;
use shell::{Shell, ShellProcess};
use std::fs;
use std::io::IsTerminal;
use std::path::Path;

const DEFAULT_PLATFORM_API: &str = ""; // match Go's empty default
const DEFAULT_EXEC_ENV: &str = "production";

#[cfg(target_family = "unix")]
const DEFAULT_LAYERS_DIR: &str = "/layers";
#[cfg(target_family = "unix")]
const DEFAULT_APP_DIR: &str = "/workspace";

#[cfg(target_family = "windows")]
const DEFAULT_LAYERS_DIR: &str = "c:\\layers";
#[cfg(target_family = "windows")]
const DEFAULT_APP_DIR: &str = "c:\\workspace";

#[cfg(target_family = "unix")]
pub const CNB_PROCESS_DIR: &str = "/cnb/process";
#[cfg(target_family = "windows")]
pub const CNB_PROCESS_DIR: &str = "c:\\cnb\\process";

#[cfg(target_family = "unix")]
pub const CNB_LIFECYCLE_DIR: &str = "/cnb/lifecycle";
#[cfg(target_family = "windows")]
pub const CNB_LIFECYCLE_DIR: &str = "c:\\cnb\\lifecycle";

fn should_enable_color() -> bool {
    if std::env::var("CNB_NO_COLOR")
        .map(|v| v.trim().to_lowercase() == "true")
        .unwrap_or(false)
    {
        return false;
    }
    std::io::stderr().is_terminal()
}

fn format_error(msg: &str, enable_color: bool) -> String {
    if enable_color {
        format!("\x1b[31;1mERROR: \x1b[0m{}", msg)
    } else {
        format!("ERROR: {}", msg)
    }
}

fn format_warning(msg: &str, enable_color: bool) -> String {
    if enable_color {
        format!("\x1b[33;1mWarning: \x1b[0m{}", msg)
    } else {
        format!("Warning: {}", msg)
    }
}

fn print_error(msg: &str) {
    eprintln!("{}", format_error(msg, should_enable_color()));
}

fn print_warning(msg: &str) {
    eprintln!("{}", format_warning(msg, should_enable_color()));
}

fn main() {
    if let Err(e) = run_launcher() {
        print_error(&e.to_string());
        std::process::exit(e.code().as_i32());
    }
}

#[derive(Debug)]
pub enum LaunchError {
    PlatformApi(api::platform::PlatformApiError),
    SetBuildpackApi {
        bp_id: String,
        error: api::buildpack::BuildpackApiError,
    },
    LaunchEnv(Box<env::LaunchEnvError>),
    ExecD(Box<exec_d::ExecDError>),
    ProcessSelection(Box<launch::ProcessSelectionError>),
    ChangeAppDir {
        path: String,
        error: std::io::Error,
    },
    MetadataNotFound {
        path: String,
    },
    MetadataRead {
        path: String,
        error: std::io::Error,
    },
    MetadataParse {
        error: toml::de::Error,
    },
    ListLayerDirs {
        context: String,
        error: std::io::Error,
    },
    ListLayerFiles {
        context: String,
        error: std::io::Error,
    },
    DirectExec(std::io::Error),
    BashExec(std::io::Error),
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let prefix = match self {
            LaunchError::PlatformApi(_)
            | LaunchError::SetBuildpackApi { .. }
            | LaunchError::MetadataNotFound { .. }
            | LaunchError::MetadataRead { .. }
            | LaunchError::MetadataParse { .. } => "failed to",
            _ => "failed to launch:",
        };

        match self {
            LaunchError::PlatformApi(api::platform::PlatformApiError::Empty) => {
                write!(
                    f,
                    "{} get platform API version; please set 'CNB_PLATFORM_API' to specify the desired platform API version",
                    prefix
                )
            }
            LaunchError::PlatformApi(api::platform::PlatformApiError::Invalid(v)) => {
                write!(f, "{} parse platform API '{}'", prefix, v)
            }
            LaunchError::PlatformApi(api::platform::PlatformApiError::Incompatible(v)) => {
                write!(
                    f,
                    "{} set platform API: platform API version '{}' is incompatible with the lifecycle",
                    prefix, v
                )
            }
            LaunchError::SetBuildpackApi { bp_id, error } => {
                write!(f, "{} set API for buildpack '{}': {}", prefix, bp_id, error)
            }
            LaunchError::LaunchEnv(err) => write!(f, "{} {}", prefix, err),
            LaunchError::ExecD(err) => write!(f, "{} {}", prefix, err),
            LaunchError::ProcessSelection(err) => write!(f, "{} {}", prefix, err),
            LaunchError::ChangeAppDir { path: _, error } => {
                write!(f, "{} change to app directory: {}", prefix, error)
            }
            LaunchError::MetadataNotFound { path } => {
                write!(
                    f,
                    "{} read metadata: metadata file not found at '{}'",
                    prefix, path
                )
            }
            LaunchError::MetadataRead { path: _, error } => {
                write!(f, "{} read metadata: {}", prefix, error)
            }
            LaunchError::MetadataParse { error } => {
                write!(f, "{} read metadata: parse failed: {}", prefix, error)
            }
            LaunchError::ListLayerDirs { context, error } => {
                write!(f, "{} {}: {}", prefix, context, error)
            }
            LaunchError::ListLayerFiles { context, error } => {
                write!(f, "{} {}: {}", prefix, context, error)
            }
            LaunchError::DirectExec(error) => {
                write!(f, "{} direct exec: {}", prefix, error)
            }
            LaunchError::BashExec(error) => {
                write!(f, "{} bash exec: {}", prefix, error)
            }
        }
    }
}

impl std::error::Error for LaunchError {}

impl LaunchError {
    pub fn code(&self) -> ExitCode {
        match self {
            LaunchError::PlatformApi(_) => ExitCode::PlatformApiIncompatible,
            LaunchError::SetBuildpackApi { .. } => ExitCode::BuildpackApiIncompatible,
            LaunchError::ProcessSelection(err) => match &**err {
                launch::ProcessSelectionError::BuildpackApi(_) => {
                    ExitCode::BuildpackApiIncompatible
                }
                _ => ExitCode::LaunchError,
            },
            _ => ExitCode::LaunchError,
        }
    }
}

fn run_launcher() -> Result<(), LaunchError> {
    // 1. Parse and verify Platform API
    let platform_api_str =
        std::env::var("CNB_PLATFORM_API").unwrap_or_else(|_| DEFAULT_PLATFORM_API.to_string());

    let platform_api =
        api::platform::verify_platform_api(&platform_api_str).map_err(LaunchError::PlatformApi)?;

    // 2. Parse Layers, App directories, and Exec Env
    let layers_dir =
        std::env::var("CNB_LAYERS_DIR").unwrap_or_else(|_| DEFAULT_LAYERS_DIR.to_string());
    let app_dir = std::env::var("CNB_APP_DIR").unwrap_or_else(|_| DEFAULT_APP_DIR.to_string());
    let exec_env = std::env::var("CNB_EXEC_ENV").unwrap_or_else(|_| DEFAULT_EXEC_ENV.to_string());

    // 3. Read metadata.toml
    let metadata_path = Path::new(&layers_dir).join("config").join("metadata.toml");
    if !metadata_path.is_file() {
        return Err(LaunchError::MetadataNotFound {
            path: metadata_path.to_string_lossy().into_owned(),
        });
    }

    let metadata_content =
        fs::read_to_string(&metadata_path).map_err(|e| LaunchError::MetadataRead {
            path: metadata_path.to_string_lossy().into_owned(),
            error: e,
        })?;

    let metadata: RawMetadata =
        toml::from_str(&metadata_content).map_err(|e| LaunchError::MetadataParse { error: e })?;

    // 4. Verify each buildpack's API
    for bp in &metadata.buildpacks {
        api::buildpack::verify_buildpack_api(&bp.id, &bp.api).map_err(|e| {
            LaunchError::SetBuildpackApi {
                bp_id: bp.id.clone(),
                error: e,
            }
        })?;
    }

    // 5. Gather CLI Arguments
    let args: Vec<String> = std::env::args().collect();

    // 6. Select the target process
    let selector = launch::ProcessSelector {
        args: &args,
        metadata: &metadata,
        platform_api: &platform_api,
        exec_env: &exec_env,
        app_dir: &app_dir,
    };

    let argv0 = args
        .first()
        .cloned()
        .unwrap_or_else(|| "launcher".to_string());

    // If they aren't using a symlink, check if they used the deprecated env variable
    let argv0_file_name = Path::new(&argv0)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "launcher".to_string());

    let process_name = if cfg!(windows) {
        argv0_file_name
            .strip_suffix(".exe")
            .unwrap_or(&argv0_file_name)
            .to_string()
    } else {
        argv0_file_name
    };

    let p_type_opt = if process_name == "launcher" {
        std::env::var("CNB_PROCESS_TYPE")
            .ok()
            .filter(|s| !s.trim().is_empty())
    } else {
        None
    };
    if let Some(p_type) = p_type_opt {
        print_warning(&format!(
            "CNB_PROCESS_TYPE is not supported in Platform API {}",
            platform_api
        ));
        print_warning(&format!(
            "Run with ENTRYPOINT '{}' to invoke the '{}' process type",
            p_type, p_type
        ));
    }

    let resolved_process = selector
        .select()
        .map_err(|e| LaunchError::ProcessSelection(Box::new(e)))?;

    // Change to app directory
    std::env::set_current_dir(&app_dir).map_err(|e| LaunchError::ChangeAppDir {
        path: app_dir.clone(),
        error: e,
    })?;

    // 8. Prepare Launch Environment
    let host_envs: Vec<(String, String)> = std::env::vars().collect();
    let mut env = LaunchEnv::new(&host_envs, CNB_PROCESS_DIR, CNB_LIFECYCLE_DIR);

    // Apply layers sequential modifications
    for bp in &metadata.buildpacks {
        let bp_dir = Path::new(&layers_dir).join(escape_id(&bp.id));
        if !bp_dir.is_dir() {
            continue;
        }

        // List and sort layer subdirectories alphabetically ascending
        let layer_dirs =
            read_layer_dirs(&bp_dir, &format!("List layers for buildpack '{}'", bp.id))?;

        // 1. Add layer roots to path variables
        for ldir in &layer_dirs {
            env.add_root_dir(ldir)
                .map_err(|e| LaunchError::LaunchEnv(Box::new(e)))?;
        }

        // 2. Add env file modifications
        for ldir in &layer_dirs {
            // Apply <layer>/env
            env.add_env_dir(ldir.join("env"), ActionType::Override)
                .map_err(|e| LaunchError::LaunchEnv(Box::new(e)))?;

            // Apply <layer>/env.launch
            env.add_env_dir(ldir.join("env.launch"), ActionType::Override)
                .map_err(|e| LaunchError::LaunchEnv(Box::new(e)))?;

            // Apply <layer>/env.launch/<process>
            if !resolved_process.proc_type.is_empty() {
                env.add_env_dir(
                    ldir.join("env.launch").join(&resolved_process.proc_type),
                    ActionType::Override,
                )
                .map_err(|e| LaunchError::LaunchEnv(Box::new(e)))?;
            }
        }
    }

    // 9. Run exec.d scripts sequentially
    for bp in &metadata.buildpacks {
        let bp_dir = Path::new(&layers_dir).join(escape_id(&bp.id));
        if !bp_dir.is_dir() {
            continue;
        }

        let layer_dirs = read_layer_dirs(&bp_dir, "List layers for exec.d")?;

        for ldir in &layer_dirs {
            let exec_d_dir = ldir.join("exec.d");
            if exec_d_dir.is_dir() {
                run_exec_d_in_dir(&exec_d_dir, &mut env)?;
            }

            if !resolved_process.proc_type.is_empty() {
                let proc_exec_d_dir = exec_d_dir.join(&resolved_process.proc_type);
                if proc_exec_d_dir.is_dir() {
                    run_exec_d_in_dir(&proc_exec_d_dir, &mut env)?;
                }
            }
        }
    }

    // 10. Execute decided process strategy
    if resolved_process.direct {
        // Direct execution (Shell-Free!)
        resolved_process
            .launch_direct(&env)
            .map_err(LaunchError::DirectExec)?;
        Ok(())
    } else {
        // Indirect execution (Invokes Bash with sourced profiles)
        let mut profiles = Vec::new();

        // Accumulate buildpack layer profiles in order
        for bp in &metadata.buildpacks {
            let bp_dir = Path::new(&layers_dir).join(escape_id(&bp.id));
            if !bp_dir.is_dir() {
                continue;
            }

            let layer_dirs = read_layer_dirs(&bp_dir, "List layers for profile.d")?;

            for ldir in &layer_dirs {
                let profile_d_dir = ldir.join("profile.d");
                if profile_d_dir.is_dir() {
                    accumulate_files_in_dir(&profile_d_dir, &mut profiles)?;
                }

                if !resolved_process.proc_type.is_empty() {
                    let proc_profile_d_dir = profile_d_dir.join(&resolved_process.proc_type);
                    if proc_profile_d_dir.is_dir() {
                        accumulate_files_in_dir(&proc_profile_d_dir, &mut profiles)?;
                    }
                }
            }
        }

        // Add app profile if it exists and is not a directory
        let profile_name = if cfg!(windows) {
            ".profile.bat"
        } else {
            ".profile"
        };
        let app_profile_path = Path::new(&app_dir).join(profile_name);
        if app_profile_path.is_file() {
            profiles.push(app_profile_path.to_string_lossy().into_owned());
        }

        let shell_proc = ShellProcess {
            script: resolved_process.args.is_empty(), // Script is true if no user args
            command: resolved_process.command.clone(),
            args: resolved_process.args.clone(),
            caller: argv0,
            profiles,
            env: env.vars().clone(),
            working_directory: resolved_process.working_directory.clone(),
        };

        #[cfg(unix)]
        let shell = BashShell;
        #[cfg(windows)]
        let shell = CmdShell;

        shell.launch(shell_proc).map_err(LaunchError::BashExec)?;
        Ok(())
    }
}

fn read_layer_dirs<P: AsRef<Path>>(
    bp_dir: P,
    error_context: &str,
) -> Result<Vec<std::path::PathBuf>, LaunchError> {
    let entries = fs::read_dir(bp_dir).map_err(|e| LaunchError::ListLayerDirs {
        context: error_context.to_string(),
        error: e,
    })?;

    let mut dirs = entries
        .map(|res| {
            res.map_err(|e| LaunchError::ListLayerDirs {
                context: error_context.to_string(),
                error: e,
            })
        })
        .filter_map(|res| match res {
            Ok(entry) => {
                if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                    Some(Ok(entry.path()))
                } else {
                    None
                }
            }
            Err(e) => Some(Err(e)),
        })
        .collect::<Result<Vec<_>, LaunchError>>()?;

    dirs.sort();
    Ok(dirs)
}

fn read_layer_files<P: AsRef<Path>>(
    dir: P,
    error_context: &str,
) -> Result<Vec<std::path::PathBuf>, LaunchError> {
    let entries = fs::read_dir(dir).map_err(|e| LaunchError::ListLayerFiles {
        context: error_context.to_string(),
        error: e,
    })?;

    let mut files = entries
        .map(|res| {
            res.map_err(|e| LaunchError::ListLayerFiles {
                context: error_context.to_string(),
                error: e,
            })
        })
        .filter_map(|res| match res {
            Ok(entry) => {
                if entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
                    Some(Ok(entry.path()))
                } else {
                    None
                }
            }
            Err(e) => Some(Err(e)),
        })
        .collect::<Result<Vec<_>, LaunchError>>()?;

    files.sort();
    Ok(files)
}

fn escape_id(id: &str) -> String {
    id.replace('/', "_")
}

fn run_exec_d_in_dir(dir: &Path, env: &mut LaunchEnv) -> Result<(), LaunchError> {
    let files = read_layer_files(dir, "List exec.d dir")?;

    for file in files {
        let res = exec_d::run_exec_d(&file, env).map_err(|e| LaunchError::ExecD(Box::new(e)))?;
        for (k, v) in res {
            env.set(&k, &v);
        }
    }
    Ok(())
}

fn accumulate_files_in_dir(dir: &Path, list: &mut Vec<String>) -> Result<(), LaunchError> {
    let files = read_layer_files(dir, "List profile.d dir")?;

    for file in files {
        list.push(file.to_string_lossy().into_owned());
    }
    Ok(())
}

#[cfg(test)]
mod main_tests {
    use super::*;

    #[test]
    fn test_format_error_output() {
        // Test non-colorized formatting
        assert_eq!(format_error("my error", false), "ERROR: my error");

        // Test colorized formatting (Bold Red sequence)
        assert_eq!(
            format_error("my error", true),
            "\x1b[31;1mERROR: \x1b[0mmy error"
        );
    }

    #[test]
    fn test_format_warning_output() {
        // Test non-colorized formatting
        assert_eq!(format_warning("my warning", false), "Warning: my warning");

        // Test colorized formatting (Bold Yellow sequence)
        assert_eq!(
            format_warning("my warning", true),
            "\x1b[33;1mWarning: \x1b[0mmy warning"
        );
    }

    #[test]
    fn test_should_enable_color_respects_env() {
        // Force color disabling via CNB_NO_COLOR
        unsafe {
            std::env::set_var("CNB_NO_COLOR", "true");
        }
        assert!(
            !should_enable_color(),
            "CNB_NO_COLOR=true must disable color"
        );

        // Remove the environment variable to restore default state
        unsafe {
            std::env::remove_var("CNB_NO_COLOR");
        }
    }

    #[test]
    fn test_escape_id() {
        assert_eq!(escape_id("heroku/ruby"), "heroku_ruby");
        assert_eq!(escape_id("no-slash"), "no-slash");
        assert_eq!(escape_id("multiple/slashes/here"), "multiple_slashes_here");
    }

    #[test]
    fn test_accumulate_files_in_dir() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path();

        // Create some files out of order
        let file_b = path.join("b.sh");
        let file_a = path.join("a.sh");
        let file_c = path.join("c.sh");

        fs::write(&file_b, "echo b").unwrap();
        fs::write(&file_a, "echo a").unwrap();
        fs::write(&file_c, "echo c").unwrap();

        let mut list = Vec::new();
        accumulate_files_in_dir(path, &mut list).unwrap();

        // Should be sorted alphabetically
        assert_eq!(list.len(), 3);
        assert_eq!(list[0], file_a.to_string_lossy().into_owned());
        assert_eq!(list[1], file_b.to_string_lossy().into_owned());
        assert_eq!(list[2], file_c.to_string_lossy().into_owned());
    }
}
