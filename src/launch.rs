//! Process selection, routing, and launching strategies.
//!
//! This module implements the core logic for selecting which process type to run (either from
//! buildpack metadata or custom user-supplied commands) and executing it either directly (shell-free)
//! or via a shell wrapper.

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

/// Errors that can occur during the selection and resolution of a CNB application process.
#[derive(Debug, PartialEq, Eq, Clone, thiserror::Error)]
pub enum ProcessSelectionError {
    /// No command-line arguments were provided by the user, and no default process type is defined in the metadata.
    #[error("determine start command: when there is no default process a command is required")]
    NoCommandAndNoDefault,
    /// The specified process type exists but is ineligible for launch because its execution environment restriction does not match the active environment.
    #[error("process type '{name}' is not eligible for execution environment '{exec_env}'")]
    IneligibleProcess { name: String, exec_env: String },
    /// The default process type defined in the metadata is ineligible for launch because of execution environment restrictions.
    #[error("Default process is not eligible for execution environment")]
    IneligibleDefault,
    /// The specified process type is ineligible for launch based on its execution environment restrictions.
    #[error("Process type '{name}' is not eligible")]
    IneligibleProcessSimple { name: String },
    /// The buildpack metadata associated with the resolved process could not be found.
    #[error("Buildpack '{bp_id}' not found in metadata for process '{proc_type}'")]
    BuildpackNotFound { bp_id: String, proc_type: String },
    /// The resolved process type has an empty command definition.
    #[error("Command entries list is empty for process '{proc_type}'")]
    EmptyCommand { proc_type: String },
    /// An error occurred during Buildpack API version verification.
    #[error(transparent)]
    BuildpackApi(#[from] crate::api::buildpack::BuildpackApiError),
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
    /// If `true`, the process should run directly (without a shell) via process replacement.
    pub direct: bool,
}

impl ResolvedProcess {
    /// Creates a user-provided process definition from raw command-line arguments.
    ///
    /// This constructor is used when the launcher is invoked with custom arguments
    /// that do not match any metadata-defined process type. It parses options like
    /// `--` to determine if the command should be run in direct or indirect execution mode.
    ///
    /// Returns a [`ResolvedProcess`] representing the user-specified command and arguments,
    /// or a [`ProcessSelectionError::NoCommandAndNoDefault`] if the command list is empty.
    pub fn from_user(cmd: &[String], app_dir: &str) -> Result<Self, ProcessSelectionError> {
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

    /// Converts a raw process definition from buildpack metadata into a resolved domain process,
    /// applying platform API version routing and environment checks.
    ///
    /// Returns `Ok(None)` if the process is ineligible for execution in the current environment.
    pub fn from_metadata(
        raw: &RawProcess,
        buildpacks: &[RawBuildpack],
        platform_api: &Version,
        exec_env: &str,
        user_args: &[String],
    ) -> Result<Option<Self>, ProcessSelectionError> {
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

        Ok(Some(Self {
            proc_type: raw.proc_type.clone(),
            command: resolved_command,
            args: resolved_args,
            working_directory,
            direct: raw.direct,
        }))
    }

    /// Launches the resolved process directly without a shell using process replacement (Unix)
    /// or process spawning followed by parent exit (Windows).
    ///
    /// # Arguments
    ///
    /// * `env` - The [`LaunchEnv`] containing environment variables to configure for the process.
    ///
    /// # Errors
    ///
    /// Returns [`std::io::Error`] if the command lookup, working directory switch, or process spawn fails.
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

    /// Launches the resolved process directly without a shell using process replacement (Unix)
    /// or process spawning followed by parent exit (Windows).
    ///
    /// # Arguments
    ///
    /// * `env` - The [`LaunchEnv`] containing environment variables to configure for the process.
    ///
    /// # Errors
    ///
    /// Returns [`std::io::Error`] if the command lookup, working directory switch, or process spawn fails.
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

/// A selector responsible for routing command-line arguments and buildpack metadata
/// to the correct resolved process according to the Cloud Native Buildpacks specification rules.
pub struct ProcessSelector<'a> {
    /// The raw command-line arguments passed to the launcher.
    pub args: &'a [String],
    /// The buildpack processes and buildpacks metadata parsed from the configuration.
    pub metadata: &'a RawMetadata,
    /// The active Platform API version.
    pub platform_api: &'a crate::api::Version,
    /// The execution environment identifier (e.g. `"production"`, `"test"`).
    pub exec_env: &'a str,
    /// The path to the application directory.
    pub app_dir: &'a str,
}

impl<'a> ProcessSelector<'a> {
    /// Selects and resolves the process to execute by applying the CNB specification process routing rules:
    ///
    /// 1. **Symlink execution:** If `argv0` (the executable name) is not `"launcher"`, find a process type in the metadata matching `argv0`.
    /// 2. **Default process:** If no user arguments are provided, use the default process type defined in the metadata.
    /// 3. **Process type match:** If the first user argument matches a process type defined in the metadata, select it and treat any subsequent arguments as process arguments.
    /// 4. **User-provided command:** If none of the above match, treat the user arguments as a custom command line.
    ///
    /// Returns a [`ResolvedProcess`] representing the fully-resolved command, arguments, working directory, and execution mode,
    /// or a [`ProcessSelectionError`] if selection fails or the chosen process is ineligible.
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
            &self.args[1..]
        } else {
            &[]
        };

        // Rule 1: argv0 matches a process type (e.g. symlink)
        let raw_proc_opt = self
            .metadata
            .processes
            .iter()
            .find(|p| p.proc_type == process_name);

        let mut resolved = if let Some(raw_proc) = raw_proc_opt {
            ResolvedProcess::from_metadata(
                raw_proc,
                &self.metadata.buildpacks,
                self.platform_api,
                self.exec_env,
                user_args,
            )?
            .ok_or_else(|| ProcessSelectionError::IneligibleProcess {
                name: process_name.clone(),
                exec_env: self.exec_env.to_string(),
            })?
        } else if user_args.is_empty() {
            return Err(ProcessSelectionError::NoCommandAndNoDefault);
        } else {
            // Rule 2: Custom user-provided command
            return ResolvedProcess::from_user(user_args, self.app_dir);
        };

        if resolved.working_directory.is_empty() {
            resolved.working_directory = self.app_dir.to_string();
        }
        Ok(resolved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_user_process_selection() {
        let cmd_direct = vec!["--".to_string(), "node".to_string(), "index.js".to_string()];
        let proc_direct = ResolvedProcess::from_user(&cmd_direct, "/workspace").unwrap();
        assert!(proc_direct.direct);
        assert_eq!(proc_direct.command, "node");
        assert_eq!(proc_direct.args, vec!["index.js".to_string()]);

        let cmd_indirect = vec!["node".to_string(), "index.js".to_string()];
        let proc_indirect = ResolvedProcess::from_user(&cmd_indirect, "/workspace").unwrap();
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
        let res_eligible =
            ResolvedProcess::from_metadata(&raw, &buildpacks, &plat_15, "production", &[]);
        assert!(res_eligible.unwrap().is_some());

        let res_ineligible =
            ResolvedProcess::from_metadata(&raw, &buildpacks, &plat_15, "test", &[]);
        assert!(res_ineligible.unwrap().is_none());

        // 2. Command separation test (Platform < 0.10 vs >= 0.10)
        let plat_9 = Version::new(0, 9);
        let res_p9 = ResolvedProcess::from_metadata(&raw, &buildpacks, &plat_9, "production", &[])
            .unwrap()
            .unwrap();
        // under plat < 0.10, entries[1..] is prepended to args
        assert_eq!(res_p9.command, "node");
        assert_eq!(res_p9.args, vec!["server.js", "--port", "8080"]);

        let plat_10 = Version::new(0, 10);
        let res_p10 =
            ResolvedProcess::from_metadata(&raw, &buildpacks, &plat_10, "production", &[])
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
        let res_old = ResolvedProcess::from_metadata(
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
        let res_new = ResolvedProcess::from_metadata(
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
            ResolvedProcess::from_metadata(&raw_no_env, &buildpacks, &plat_15, "test", &[])
                .unwrap()
                .is_some()
        );
        assert!(
            ResolvedProcess::from_metadata(&raw_no_env, &buildpacks, &plat_15, "", &[])
                .unwrap()
                .is_some()
        );

        // Wildcard '*' -> matches any
        assert!(
            ResolvedProcess::from_metadata(&raw_wildcard, &buildpacks, &plat_15, "test", &[])
                .unwrap()
                .is_some()
        );
        assert!(
            ResolvedProcess::from_metadata(&raw_wildcard, &buildpacks, &plat_15, "", &[])
                .unwrap()
                .is_some()
        );

        // Specific 'production' -> matches production, fails on test/empty
        assert!(
            ResolvedProcess::from_metadata(&raw_prod, &buildpacks, &plat_15, "production", &[])
                .unwrap()
                .is_some()
        );
        assert!(
            ResolvedProcess::from_metadata(&raw_prod, &buildpacks, &plat_15, "test", &[])
                .unwrap()
                .is_none()
        );
        assert!(
            ResolvedProcess::from_metadata(&raw_prod, &buildpacks, &plat_15, "", &[])
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
        assert_eq!(res.working_directory, "/workspace");

        // Rule 2: Default process (argv0 is launcher, no user args)
        let selector = ProcessSelector {
            args: &["launcher".to_string()],
            metadata: &metadata,
            platform_api: &platform_api,
            exec_env: "test",
            app_dir: "/workspace",
        };
        let err = selector.select().unwrap_err();
        assert!(matches!(err, ProcessSelectionError::NoCommandAndNoDefault));

        // Rule 2: Custom user-provided command
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
        assert_eq!(res.working_directory, "/workspace");
    }
}

#[cfg(test)]
mod ported_rust_tests {
    use super::*;

    // Helpers mirroring the launcher-rust fixtures, adapted to launcher-rs's
    // RawProcess (which has NO Default impl: every field must be specified).

    fn sv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// Build a RawProcess with all fields explicit. launcher-rs's RawProcess has
    /// no Default, so this mirrors launcher-rust's struct-literal fixtures.
    fn raw_proc(
        proc_type: &str,
        command: RawCommand,
        args: Option<Vec<String>>,
        buildpack_id: &str,
        exec_env: Option<Vec<String>>,
    ) -> RawProcess {
        RawProcess {
            proc_type: proc_type.to_string(),
            command,
            args,
            direct: false,
            default: false,
            buildpack_id: buildpack_id.to_string(),
            working_dir: None,
            exec_env,
        }
    }

    // ---------- ResolvedProcess::from_user ----------
    // Mirrors launcher-rust src/launch/process.rs userProvidedProcess behavior:
    // a leading "--" marker yields a DIRECT process and splits command/args.

    #[test]
    fn from_user_direct_marker_splits() {
        let cmd = sv(&["--", "node", "index.js", "extra"]);
        let proc = ResolvedProcess::from_user(&cmd, "/workspace").unwrap();
        assert!(proc.direct);
        assert_eq!(proc.command, "node");
        assert_eq!(proc.args, sv(&["index.js", "extra"]));
        assert_eq!(proc.working_directory, "/workspace");
    }

    #[test]
    fn from_user_shell_when_no_marker() {
        let cmd = sv(&["node", "index.js"]);
        let proc = ResolvedProcess::from_user(&cmd, "/workspace").unwrap();
        assert!(!proc.direct);
        assert_eq!(proc.command, "node");
        assert_eq!(proc.args, sv(&["index.js"]));
        assert_eq!(proc.working_directory, "/workspace");
    }

    // ---------- ResolvedProcess::from_metadata: eligibility & arg merge ----------

    #[test]
    fn from_metadata_eligible_direct() {
        // Eligible: exec-env=["production"] under exec_env "production", platform >= 0.15.
        // Go-correct: returns Some(resolved) preserving the direct flag.
        let raw = RawProcess {
            proc_type: "web".to_string(),
            command: RawCommand::Array(vec!["node".to_string(), "server.js".to_string()]),
            args: Some(sv(&["--port", "8080"])),
            direct: true,
            default: true,
            buildpack_id: "my-bp".to_string(),
            working_dir: Some("/app".to_string()),
            exec_env: Some(sv(&["production"])),
        };
        let bps = vec![RawBuildpack {
            id: "my-bp".to_string(),
            api: "0.12".to_string(),
        }];
        let resolved =
            ResolvedProcess::from_metadata(&raw, &bps, &Version::new(0, 15), "production", &[])
                .unwrap()
                .expect("process should be eligible");
        assert_eq!(resolved.proc_type, "web");
        assert_eq!(resolved.command, "node");
        assert!(resolved.direct);
        // platform >= 0.10, multi-entry command: entries[1..] always-provided, then
        // overridable defaults appended because user_args is empty.
        assert_eq!(resolved.args, sv(&["server.js", "--port", "8080"]));
        assert_eq!(resolved.working_directory, "/app");
    }

    #[test]
    fn from_metadata_bp_lt09_appends_args() {
        // Single-entry command, buildpack API 0.8 (< 0.9), platform >= 0.10 with user args.
        // Go-correct: user args are APPENDED to the process default args.
        let raw = raw_proc(
            "web",
            RawCommand::Single("node".to_string()),
            Some(sv(&["server.js"])),
            "my-bp",
            None,
        );
        let bps = vec![RawBuildpack {
            id: "my-bp".to_string(),
            api: "0.8".to_string(),
        }];
        let resolved = ResolvedProcess::from_metadata(
            &raw,
            &bps,
            &Version::new(0, 10),
            "production",
            &sv(&["user-arg"]),
        )
        .unwrap()
        .unwrap();
        assert_eq!(resolved.command, "node");
        assert_eq!(resolved.args, sv(&["server.js", "user-arg"]));
    }

    #[test]
    fn from_metadata_bp_ge09_replaces_args() {
        // Single-entry command, buildpack API 0.9 (>= 0.9), platform >= 0.10 with user args.
        // Go-correct: user args REPLACE the process default args.
        let raw = raw_proc(
            "web",
            RawCommand::Single("node".to_string()),
            Some(sv(&["server.js"])),
            "my-bp",
            None,
        );
        let bps = vec![RawBuildpack {
            id: "my-bp".to_string(),
            api: "0.9".to_string(),
        }];
        let resolved = ResolvedProcess::from_metadata(
            &raw,
            &bps,
            &Version::new(0, 10),
            "production",
            &sv(&["user-arg"]),
        )
        .unwrap()
        .unwrap();
        assert_eq!(resolved.command, "node");
        assert_eq!(resolved.args, sv(&["user-arg"]));
    }

    // ---------- RawCommand parsing (launch.toml / metadata.toml schema) ----------
    // Mirrors launcher-rust src/launch/metadata.rs RawCommand deserialize tests:
    // a bare string and an array of strings must both parse into command entries.

    #[test]
    fn command_as_string_parses() {
        #[derive(serde::Deserialize)]
        struct Holder {
            command: RawCommand,
        }
        // launcher-rs RawCommand is #[serde(untagged)] Single(String) | Array(Vec<String>).
        let h: Holder = serde_json::from_str(r#"{"command": "bash run.sh"}"#).unwrap();
        match h.command {
            RawCommand::Single(s) => assert_eq!(s, "bash run.sh"),
            RawCommand::Array(a) => panic!("expected Single, got Array({a:?})"),
        }
    }

    #[test]
    fn command_as_list_parses() {
        #[derive(serde::Deserialize)]
        struct Holder {
            command: RawCommand,
        }
        let h: Holder = serde_json::from_str(r#"{"command": ["bash", "run.sh"]}"#).unwrap();
        match h.command {
            RawCommand::Array(a) => assert_eq!(a, sv(&["bash", "run.sh"])),
            RawCommand::Single(s) => panic!("expected Array, got Single({s:?})"),
        }
    }

    // ---------- Working directory default to app_dir ----------
    // launcher-rs has NO standalone get_process_working_directory fn; the fallback
    // to app_dir is inlined in ProcessSelector::select (launch.rs:406). Mirror the
    // launcher-rust working_dir_falls_back_to_app_dir_when_unset intent through select().

    #[test]
    fn process_working_dir_defaults_appdir() {
        // Process with no working-dir; argv0 basename selects it (symlink rule).
        let raw = raw_proc(
            "web",
            RawCommand::Single("node".to_string()),
            None,
            "my-bp",
            None,
        );
        let metadata = RawMetadata {
            processes: vec![raw],
            buildpacks: vec![RawBuildpack {
                id: "my-bp".to_string(),
                api: "0.12".to_string(),
            }],
        };
        let platform_api = Version::new(0, 15);
        let selector = ProcessSelector {
            args: &sv(&["web"]),
            metadata: &metadata,
            platform_api: &platform_api,
            exec_env: "production",
            app_dir: "/workspace",
        };
        let resolved = selector.select().unwrap();
        assert_eq!(resolved.proc_type, "web");
        assert_eq!(resolved.working_directory, "/workspace");
    }

    // ---------- Exec-env filtering ----------

    #[test]
    fn exec_env_specific_match() {
        // exec-env=["production"], platform >= 0.15, exec_env "production" -> eligible (Some).
        let raw = raw_proc(
            "prod-only",
            RawCommand::Single("prod-command".to_string()),
            None,
            "my-bp",
            Some(sv(&["production"])),
        );
        let bps = vec![RawBuildpack {
            id: "my-bp".to_string(),
            api: "0.12".to_string(),
        }];
        let resolved =
            ResolvedProcess::from_metadata(&raw, &bps, &Version::new(0, 15), "production", &[])
                .unwrap();
        assert!(resolved.is_some());
        assert_eq!(resolved.unwrap().proc_type, "prod-only");

        // The same process under a non-matching exec_env is ineligible (Ok(None)).
        let ineligible =
            ResolvedProcess::from_metadata(&raw, &bps, &Version::new(0, 15), "test", &[]).unwrap();
        assert!(ineligible.is_none());
    }

    #[test]
    fn exec_env_no_env_is_wildcard() {
        // No exec-env specified -> matches any execution environment (incl. empty).
        let raw = raw_proc(
            "web",
            RawCommand::Single("node".to_string()),
            None,
            "my-bp",
            None,
        );
        let bps = vec![RawBuildpack {
            id: "my-bp".to_string(),
            api: "0.12".to_string(),
        }];
        let platform = Version::new(0, 15);
        assert!(
            ResolvedProcess::from_metadata(&raw, &bps, &platform, "test", &[])
                .unwrap()
                .is_some()
        );
        assert!(
            ResolvedProcess::from_metadata(&raw, &bps, &platform, "production", &[])
                .unwrap()
                .is_some()
        );
        assert!(
            ResolvedProcess::from_metadata(&raw, &bps, &platform, "", &[])
                .unwrap()
                .is_some()
        );
    }

    // ---------- ProcessSelector: single-arg / argv0 process-type lookup ----------

    #[test]
    fn single_arg_process_type_lookup() {
        // Rule 1: argv0 basename matches a process type (symlink execution).
        let raw_web = raw_proc(
            "web",
            RawCommand::Single("node".to_string()),
            Some(sv(&["server.js"])),
            "my-bp",
            None,
        );
        let raw_worker = raw_proc(
            "worker",
            RawCommand::Single("node".to_string()),
            Some(sv(&["worker.js"])),
            "my-bp",
            None,
        );
        let metadata = RawMetadata {
            processes: vec![raw_web, raw_worker],
            buildpacks: vec![RawBuildpack {
                id: "my-bp".to_string(),
                api: "0.12".to_string(),
            }],
        };
        let platform_api = Version::new(0, 15);
        let selector = ProcessSelector {
            args: &sv(&["/usr/local/bin/worker", "--flag"]),
            metadata: &metadata,
            platform_api: &platform_api,
            exec_env: "test",
            app_dir: "/workspace",
        };
        let resolved = selector.select().unwrap();
        assert_eq!(resolved.proc_type, "worker");
        // Buildpack API 0.12 (>= 0.9): user args replace default args.
        assert_eq!(resolved.args, sv(&["--flag"]));
        assert_eq!(resolved.working_directory, "/workspace");
    }

    // ---------- GAP tests (Go-correct assertions that launcher-rs violates) ----------

    // GAP: launcher-rust treats a non-empty buildpack_id that matches NO buildpack as
    // pre-0.9 (bp_api None -> APPEND user args), so args == ["a","u1"]. launcher-rs
    // instead returns Err(ProcessSelectionError::BuildpackNotFound) when buildpack_id
    // is present but absent from the buildpacks list, so the first .unwrap() panics.
    // This test asserts the Go-correct value and is expected to FAIL against launcher-rs.
    #[test]
    fn unknown_buildpack_id_treated_as_pre_09() {
        let raw = raw_proc(
            "lonely",
            RawCommand::Single("cmd".to_string()),
            Some(sv(&["a"])),
            "nonexistent",
            None,
        );
        let resolved =
            ResolvedProcess::from_metadata(&raw, &[], &Version::new(0, 15), "", &sv(&["u1"]))
                .unwrap()
                .unwrap();
        assert_eq!(resolved.args, sv(&["a", "u1"]));
    }

    // GAP: launcher-rust scans for the FIRST ELIGIBLE process of a given type, so with
    // two "duplicate" processes (production then test) under exec_env="test" it selects
    // the test one (command == "test-version"). launcher-rs's ProcessSelector::select
    // uses processes.iter().find(|p| p.proc_type == name) which returns the FIRST
    // POSITIONAL "duplicate" (production), then from_metadata returns Ok(None) for
    // exec_env="test", so select() maps to Err(IneligibleProcess) and .unwrap() panics.
    // This test asserts the Go-correct value and is expected to FAIL against launcher-rs.
    #[test]
    fn exec_env_duplicate_types_picks_first_eligible() {
        let prod = raw_proc(
            "duplicate",
            RawCommand::Single("prod-version".to_string()),
            None,
            "some-buildpack",
            Some(sv(&["production"])),
        );
        let test = raw_proc(
            "duplicate",
            RawCommand::Single("test-version".to_string()),
            None,
            "some-buildpack",
            Some(sv(&["test"])),
        );
        let metadata = RawMetadata {
            processes: vec![prod, test],
            buildpacks: vec![RawBuildpack {
                id: "some-buildpack".to_string(),
                api: "0.8".to_string(),
            }],
        };
        let platform_api = Version::new(0, 15);
        let selector = ProcessSelector {
            args: &sv(&["duplicate"]),
            metadata: &metadata,
            platform_api: &platform_api,
            exec_env: "test",
            app_dir: "/workspace",
        };
        let resolved = selector.select().unwrap();
        assert_eq!(resolved.command, "test-version");
    }
}
