use crate::api::Version;
use crate::env::LaunchEnv;
use serde::Deserialize;
use std::path::Path;
use std::process::Command;

/// Represents a raw command format from the metadata.toml.
/// Can be a single string or an array of string tokens.
#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum RawCommand {
    /// A single shell command string (e.g. `"echo hello"`).
    Single(String),
    /// An array of process tokens (e.g. `["echo", "hello"]`).
    Array(Vec<String>),
}

/// Represents the raw buildpack metadata read from layers/config/metadata.toml.
#[derive(Debug, Deserialize, Clone)]
pub struct RawBuildpack {
    /// Unique identifier of the buildpack.
    pub id: String,
    /// Buildpack API version claimed by the buildpack.
    pub api: String,
}

/// Represents the raw process definition parsed directly from layers/config/metadata.toml.
#[derive(Debug, Deserialize, Clone)]
pub struct RawProcess {
    /// The process type identifier (e.g. `"web"`, `"worker"`).
    #[serde(rename = "type")]
    pub proc_type: String,
    /// The executable command or script.
    pub command: RawCommand,
    /// Optional default arguments.
    pub args: Option<Vec<String>>,
    /// Determines whether the process runs directly without a shell.
    pub direct: bool,
    /// If `true`, this process serves as the default startup target.
    #[serde(default)]
    pub default: bool,
    /// The ID of the buildpack that contributed this process.
    #[serde(rename = "buildpack-id", default)]
    pub buildpack_id: String,
    /// Custom working directory for the process.
    #[serde(rename = "working-dir")]
    pub working_dir: Option<String>,
    /// Optional execution environment filter flags (supported in Platform API >= 0.15).
    #[serde(rename = "exec-env")]
    pub exec_env: Option<Vec<String>>,
}

/// Represents the top-level sandbox metadata schema parsed from layers/config/metadata.toml.
#[derive(Debug, Deserialize, Clone)]
pub struct RawMetadata {
    /// All processes contributed by buildpacks.
    pub processes: Vec<RawProcess>,
    /// The set of buildpacks used to build the application.
    pub buildpacks: Vec<RawBuildpack>,
}

/// A fully resolved, version-agnostic domain process model.
/// This struct hides all version gates and platform differences behind the boundary parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProcess {
    /// The process type identifier.
    pub proc_type: String,
    /// The resolved absolute path of the executable command.
    pub command: String,
    /// The final, fully-resolved argument slice.
    pub args: Vec<String>,
    /// The resolved working directory.
    pub working_directory: String,
    pub direct: bool,
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum ProcessSelectionError {
    NoCommandAndNoDefault,
    IneligibleProcess { name: String, exec_env: String },
    IneligibleDefault,
    IneligibleProcessSimple { name: String },
    BuildpackNotFound { bp_id: String, proc_type: String },
    EmptyCommand { proc_type: String },
    BuildpackApi(crate::api::buildpack::BuildpackApiError),
}

impl std::fmt::Display for ProcessSelectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProcessSelectionError::NoCommandAndNoDefault => {
                write!(
                    f,
                    "determine start command: when there is no default process a command is required"
                )
            }
            ProcessSelectionError::IneligibleProcess { name, exec_env } => {
                write!(
                    f,
                    "process type '{}' is not eligible for execution environment '{}'",
                    name, exec_env
                )
            }
            ProcessSelectionError::IneligibleDefault => {
                write!(
                    f,
                    "Default process is not eligible for execution environment"
                )
            }
            ProcessSelectionError::IneligibleProcessSimple { name } => {
                write!(f, "Process type '{}' is not eligible", name)
            }
            ProcessSelectionError::BuildpackNotFound { bp_id, proc_type } => {
                write!(
                    f,
                    "Buildpack '{}' not found in metadata for process '{}'",
                    bp_id, proc_type
                )
            }
            ProcessSelectionError::EmptyCommand { proc_type } => {
                write!(
                    f,
                    "Command entries list is empty for process '{}'",
                    proc_type
                )
            }
            ProcessSelectionError::BuildpackApi(err) => {
                write!(f, "{}", err)
            }
        }
    }
}

impl std::error::Error for ProcessSelectionError {}

impl From<crate::api::buildpack::BuildpackApiError> for ProcessSelectionError {
    fn from(err: crate::api::buildpack::BuildpackApiError) -> Self {
        ProcessSelectionError::BuildpackApi(err)
    }
}

impl ResolvedProcess {
    /// Creates a user-provided process definition from raw command-line arguments.
    pub fn user_provided(cmd: &[String], app_dir: &str) -> Result<Self, ProcessSelectionError> {
        if cmd.is_empty() {
            return Err(ProcessSelectionError::NoCommandAndNoDefault);
        }

        if cmd.len() > 1 && cmd[0] == "--" {
            Ok(ResolvedProcess {
                proc_type: "".to_string(),
                command: cmd[1].clone(),
                args: cmd[2..].to_vec(),
                working_directory: app_dir.to_string(),
                direct: true,
            })
        } else {
            Ok(ResolvedProcess {
                proc_type: "".to_string(),
                command: cmd[0].clone(),
                args: cmd[1..].to_vec(),
                working_directory: app_dir.to_string(),
                direct: false,
            })
        }
    }

    /// Launches a process directly (without a shell) using Unix process replacement.
    #[cfg(unix)]
    pub fn launch_direct(&self, env: &LaunchEnv) -> Result<(), std::io::Error> {
        use std::os::unix::process::CommandExt;

        let path_val = env.get("PATH").cloned().unwrap_or_default();

        // Find the absolute path to the command
        let binary_path =
            which::which_in(&self.command, Some(&path_val), &std::env::current_dir()?).map_err(
                |e| {
                    std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        format!("Path lookup failed for '{}': {}", self.command, e),
                    )
                },
            )?;

        // Change directory to the process working directory
        let work_dir = if self.working_directory.is_empty() {
            Path::new(".")
        } else {
            Path::new(&self.working_directory)
        };
        std::env::set_current_dir(work_dir)?;

        let mut cmd = Command::new(binary_path);
        cmd.args(&self.args);
        cmd.env_clear();
        cmd.envs(env.vars());

        let err = cmd.exec();
        Err(err)
    }

    /// Launches a process directly (without a shell) on Windows.
    #[cfg(windows)]
    pub fn launch_direct(&self, env: &LaunchEnv) -> Result<(), std::io::Error> {
        let path_val = env.get("PATH").cloned().unwrap_or_default();

        // Find the absolute path to the command
        let binary_path =
            which::which_in(&self.command, Some(&path_val), &std::env::current_dir()?).map_err(
                |e| {
                    std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        format!("Path lookup failed for '{}': {}", self.command, e),
                    )
                },
            )?;

        // Change directory to the process working directory
        let work_dir = if self.working_directory.is_empty() {
            Path::new(".")
        } else {
            Path::new(&self.working_directory)
        };
        std::env::set_current_dir(work_dir)?;

        let mut cmd = Command::new(binary_path);
        cmd.args(&self.args);
        cmd.env_clear();
        cmd.envs(env.vars());

        let mut child = cmd.spawn()?;
        let status = child.wait()?;
        std::process::exit(status.code().unwrap_or(0));
    }
}

pub struct ProcessSelector<'a> {
    pub args: &'a [String],
    pub metadata: &'a RawMetadata,
    pub platform_api: &'a crate::api::Version,
    pub exec_env: &'a str,
    pub app_dir: &'a str,
}

impl<'a> ProcessSelector<'a> {
    pub fn select(self) -> Result<ResolvedProcess, ProcessSelectionError> {
        let argv0 = self.args.first().map(|s| s.as_str()).unwrap_or("launcher");

        let argv0_file_name = std::path::Path::new(argv0)
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

        let user_args = if self.args.len() > 1 {
            self.args[1..].to_vec()
        } else {
            Vec::new()
        };

        // Rule 1: argv0 matches a process type (e.g. symlink)
        let raw_proc_opt = if process_name != "launcher" {
            self.metadata.processes.iter().find(|p| p.proc_type == process_name)
        } else {
            None
        };
        if let Some(raw_proc) = raw_proc_opt {
            return resolve_process(
                raw_proc,
                &self.metadata.buildpacks,
                self.platform_api,
                self.exec_env,
                &user_args,
            )?
            .ok_or_else(|| ProcessSelectionError::IneligibleProcess {
                name: process_name.clone(),
                exec_env: self.exec_env.to_string(),
            });
        }

        if user_args.is_empty() {
            // Rule 2: Default process from metadata
            if let Some(raw_proc) = self.metadata.processes.iter().find(|p| p.default) {
                return resolve_process(
                    raw_proc,
                    &self.metadata.buildpacks,
                    self.platform_api,
                    self.exec_env,
                    &[],
                )?
                .ok_or(ProcessSelectionError::IneligibleDefault);
            }
            return Err(ProcessSelectionError::NoCommandAndNoDefault);
        }

        // Rule 3: user_args[0] matches a process type (fallback mechanism)
        if let Some(raw_proc) = self
            .metadata
            .processes
            .iter()
            .find(|p| p.proc_type == user_args[0])
        {
            return resolve_process(
                raw_proc,
                &self.metadata.buildpacks,
                self.platform_api,
                self.exec_env,
                &user_args[1..],
            )?
            .ok_or_else(|| ProcessSelectionError::IneligibleProcessSimple {
                name: user_args[0].clone(),
            });
        }

        // Rule 4: Custom user-provided command
        ResolvedProcess::user_provided(&user_args, self.app_dir)
    }
}

/// Upfront translator/boundary parser. Converts raw process & metadata to a ResolvedProcess.
/// Returns Ok(None) if the process is filtered out due to exec-env eligibility.
pub fn resolve_process(
    raw: &RawProcess,
    buildpacks: &[RawBuildpack],
    platform_api: &Version,
    exec_env: &str,
    user_args: &[String],
) -> Result<Option<ResolvedProcess>, ProcessSelectionError> {
    // 1. Check eligibility (Platform API >= 0.15)
    if platform_api.at_least("0.15")
        && raw.exec_env.as_ref().is_some_and(|envs| {
            !envs.is_empty()
                && !envs.contains(&"*".to_string())
                && !envs.contains(&exec_env.to_string())
        })
    {
        return Ok(None); // Ineligible
    }

    // 2. Find buildpack and verify buildpack API
    let bp_api = if !raw.buildpack_id.is_empty() {
        let bp = buildpacks
            .iter()
            .find(|bp| bp.id == raw.buildpack_id)
            .ok_or_else(|| ProcessSelectionError::BuildpackNotFound {
                bp_id: raw.buildpack_id.clone(),
                proc_type: raw.proc_type.clone(),
            })?;
        Some(crate::api::buildpack::verify_buildpack_api(
            &bp.id, &bp.api,
        )?)
    } else {
        None
    };

    // 3. Resolve command and arguments
    let entries = match &raw.command {
        RawCommand::Single(cmd) => vec![cmd.clone()],
        RawCommand::Array(arr) => arr.clone(),
    };

    if entries.is_empty() {
        return Err(ProcessSelectionError::EmptyCommand {
            proc_type: raw.proc_type.clone(),
        });
    }

    let resolved_command = entries[0].clone();
    let mut resolved_args = Vec::new();

    if platform_api.less_than("0.10") {
        // Under platform < 0.10, we only support a single command string.
        // Any overflow entries in RawCommand are pushed to arguments list.
        resolved_args.extend(entries[1..].iter().cloned());
        resolved_args.extend(raw.args.clone().unwrap_or_default());
        resolved_args.extend(user_args.iter().cloned());
    } else {
        // Platform >= 0.10
        if entries.len() > 1 {
            // Definitely newer buildpack command array
            resolved_args.extend(entries[1..].iter().cloned()); // always-provided args
            if !user_args.is_empty() {
                resolved_args.extend(user_args.iter().cloned());
            } else {
                resolved_args.extend(raw.args.clone().unwrap_or_default()); // overridable default args
            }
        } else {
            // Single entry command
            if user_args.is_empty() {
                resolved_args.extend(raw.args.clone().unwrap_or_default());
            } else {
                let is_bp_less_than_09 = if let Some(bp_ver) = bp_api {
                    bp_ver.less_than("0.9")
                } else {
                    false
                };

                if is_bp_less_than_09 {
                    resolved_args.extend(raw.args.clone().unwrap_or_default());
                    resolved_args.extend(user_args.iter().cloned());
                } else {
                    resolved_args.extend(user_args.iter().cloned()); // replaces completely
                }
            }
        }
    }

    let working_directory = raw.working_dir.clone().unwrap_or_default();

    Ok(Some(ResolvedProcess {
        proc_type: raw.proc_type.clone(),
        command: resolved_command,
        args: resolved_args,
        working_directory,
        direct: raw.direct,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_user_provided_process_selection() {
        let cmd_direct = vec!["--".to_string(), "node".to_string(), "index.js".to_string()];
        let proc_direct = ResolvedProcess::user_provided(&cmd_direct, "/workspace").unwrap();
        assert!(proc_direct.direct);
        assert_eq!(proc_direct.command, "node");
        assert_eq!(proc_direct.args, vec!["index.js".to_string()]);

        let cmd_indirect = vec!["node".to_string(), "index.js".to_string()];
        let proc_indirect = ResolvedProcess::user_provided(&cmd_indirect, "/workspace").unwrap();
        assert!(!proc_indirect.direct);
        assert_eq!(proc_indirect.command, "node");
        assert_eq!(proc_indirect.args, vec!["index.js".to_string()]);
    }

    #[test]
    fn test_resolve_process_gates() {
        let raw = RawProcess {
            proc_type: "web".to_string(),
            command: RawCommand::Array(vec!["node".to_string(), "server.js".to_string()]),
            args: Some(vec!["--port".to_string(), "8080".to_string()]),
            direct: true,
            default: true,
            buildpack_id: "my-bp".to_string(),
            working_dir: Some("/app".to_string()),
            exec_env: Some(vec!["production".to_string()]),
        };

        let buildpacks = vec![RawBuildpack {
            id: "my-bp".to_string(),
            api: "0.12".to_string(),
        }];

        // 1. Eligibility test (Platform >= 0.15)
        let plat_15 = Version::new(0, 15);
        let res_eligible = resolve_process(&raw, &buildpacks, &plat_15, "production", &[]);
        assert!(res_eligible.unwrap().is_some());

        let res_ineligible = resolve_process(&raw, &buildpacks, &plat_15, "test", &[]);
        assert!(res_ineligible.unwrap().is_none());

        // 2. Command separation test (Platform < 0.10 vs >= 0.10)
        let plat_9 = Version::new(0, 9);
        let res_p9 = resolve_process(&raw, &buildpacks, &plat_9, "production", &[])
            .unwrap()
            .unwrap();
        // under plat < 0.10, entries[1..] is prepended to args
        assert_eq!(res_p9.command, "node");
        assert_eq!(res_p9.args, vec!["server.js", "--port", "8080"]);

        let plat_10 = Version::new(0, 10);
        let res_p10 = resolve_process(&raw, &buildpacks, &plat_10, "production", &[])
            .unwrap()
            .unwrap();
        assert_eq!(res_p10.command, "node");
        // under plat >= 0.10, entries[1..] are always-provided, args are defaults (and since user_args is empty, they are appended)
        assert_eq!(res_p10.args, vec!["server.js", "--port", "8080"]);
    }

    #[test]
    fn test_resolve_process_args_replacement() {
        let raw = RawProcess {
            proc_type: "web".to_string(),
            command: RawCommand::Single("node".to_string()),
            args: Some(vec!["server.js".to_string()]),
            direct: true,
            default: true,
            buildpack_id: "my-bp".to_string(),
            working_dir: None,
            exec_env: None,
        };

        // 1. Buildpack API < 0.9: user args are appended
        let bps_old = vec![RawBuildpack {
            id: "my-bp".to_string(),
            api: "0.8".to_string(),
        }];
        let plat = Version::new(0, 10);
        let res_old = resolve_process(
            &raw,
            &bps_old,
            &plat,
            "production",
            &["user-arg".to_string()],
        )
        .unwrap()
        .unwrap();
        assert_eq!(res_old.args, vec!["server.js", "user-arg"]);

        // 2. Buildpack API >= 0.9: user args replace process args
        let bps_new = vec![RawBuildpack {
            id: "my-bp".to_string(),
            api: "0.9".to_string(),
        }];
        let res_new = resolve_process(
            &raw,
            &bps_new,
            &plat,
            "production",
            &["user-arg".to_string()],
        )
        .unwrap()
        .unwrap();
        assert_eq!(res_new.args, vec!["user-arg"]);
    }

    #[test]
    fn test_exec_env_matching() {
        let raw_no_env = RawProcess {
            proc_type: "web".to_string(),
            command: RawCommand::Single("node".to_string()),
            args: None,
            direct: true,
            default: true,
            buildpack_id: "my-bp".to_string(),
            working_dir: None,
            exec_env: None,
        };

        let raw_wildcard = RawProcess {
            proc_type: "web".to_string(),
            command: RawCommand::Single("node".to_string()),
            args: None,
            direct: true,
            default: true,
            buildpack_id: "my-bp".to_string(),
            working_dir: None,
            exec_env: Some(vec!["*".to_string()]),
        };

        let raw_prod = RawProcess {
            proc_type: "web".to_string(),
            command: RawCommand::Single("node".to_string()),
            args: None,
            direct: true,
            default: true,
            buildpack_id: "my-bp".to_string(),
            working_dir: None,
            exec_env: Some(vec!["production".to_string()]),
        };

        let buildpacks = vec![RawBuildpack {
            id: "my-bp".to_string(),
            api: "0.12".to_string(),
        }];

        let plat_15 = Version::new(0, 15);

        // No exec-env -> matches any
        assert!(
            resolve_process(&raw_no_env, &buildpacks, &plat_15, "test", &[])
                .unwrap()
                .is_some()
        );
        assert!(
            resolve_process(&raw_no_env, &buildpacks, &plat_15, "", &[])
                .unwrap()
                .is_some()
        );

        // Wildcard '*' -> matches any
        assert!(
            resolve_process(&raw_wildcard, &buildpacks, &plat_15, "test", &[])
                .unwrap()
                .is_some()
        );
        assert!(
            resolve_process(&raw_wildcard, &buildpacks, &plat_15, "", &[])
                .unwrap()
                .is_some()
        );

        // Specific 'production' -> matches production, fails on test/empty
        assert!(
            resolve_process(&raw_prod, &buildpacks, &plat_15, "production", &[])
                .unwrap()
                .is_some()
        );
        assert!(
            resolve_process(&raw_prod, &buildpacks, &plat_15, "test", &[])
                .unwrap()
                .is_none()
        );
        assert!(
            resolve_process(&raw_prod, &buildpacks, &plat_15, "", &[])
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn test_process_selector() {
        let raw_web = RawProcess {
            proc_type: "web".to_string(),
            command: RawCommand::Single("node".to_string()),
            args: Some(vec!["server.js".to_string()]),
            direct: true,
            default: true,
            buildpack_id: "my-bp".to_string(),
            working_dir: None,
            exec_env: None,
        };

        let raw_worker = RawProcess {
            proc_type: "worker".to_string(),
            command: RawCommand::Single("node".to_string()),
            args: Some(vec!["worker.js".to_string()]),
            direct: true,
            default: false,
            buildpack_id: "my-bp".to_string(),
            working_dir: None,
            exec_env: None,
        };

        let metadata = RawMetadata {
            processes: vec![raw_web, raw_worker],
            buildpacks: vec![RawBuildpack {
                id: "my-bp".to_string(),
                api: "0.12".to_string(),
            }],
        };

        let platform_api = Version::new(0, 15);

        // Rule 1: argv0 matches process type (symlink execution)
        let selector = ProcessSelector {
            args: &[
                "/usr/local/bin/worker".to_string(), // argv0
                "--flag".to_string(),                // user args
            ],
            metadata: &metadata,
            platform_api: &platform_api,
            exec_env: "test",
            app_dir: "/workspace",
        };
        let res = selector.select().unwrap();
        assert_eq!(res.proc_type, "worker");
        assert_eq!(res.args, vec!["--flag".to_string()]); // API >= 0.9 user args replace default args

        // Rule 2: Default process (argv0 is launcher, no user args)
        let selector = ProcessSelector {
            args: &["launcher".to_string()],
            metadata: &metadata,
            platform_api: &platform_api,
            exec_env: "test",
            app_dir: "/workspace",
        };
        let res = selector.select().unwrap();
        assert_eq!(res.proc_type, "web"); // web is default

        // Rule 3: user_args[0] matches process type
        let selector = ProcessSelector {
            args: &[
                "launcher".to_string(),
                "worker".to_string(), // user args[0]
            ],
            metadata: &metadata,
            platform_api: &platform_api,
            exec_env: "test",
            app_dir: "/workspace",
        };
        let res = selector.select().unwrap();
        assert_eq!(res.proc_type, "worker");

        // Rule 4: Custom user-provided command
        let selector = ProcessSelector {
            args: &[
                "launcher".to_string(),
                "python".to_string(),
                "script.py".to_string(),
            ],
            metadata: &metadata,
            platform_api: &platform_api,
            exec_env: "test",
            app_dir: "/workspace",
        };
        let res = selector.select().unwrap();
        assert_eq!(res.command, "python");
        assert_eq!(res.args, vec!["script.py".to_string()]);
    }
}
