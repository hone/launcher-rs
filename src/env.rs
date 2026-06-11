//! Environment variables management and CNB compliance.
//!
//! This module manages variables during launcher initialization and execution. It is responsible
//! for purging forbidden variables from the host environment, sanitizing path-like lists (e.g., `PATH`
//! and `LD_LIBRARY_PATH`), and loading layered configuration directories (`env`, `env.launch`, and
//! `env.launch/<process>`) according to the Cloud Native Buildpacks specification rules.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Represents the environment modification action type defined by the CNB specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionType {
    /// Overwrites any existing value of the variable.
    Override,
    /// Sets the variable only if it does not already exist.
    Default,
    /// Appends the new value to the end of the variable, using an optional custom delimiter.
    Append,
    /// Prepends the new value to the beginning of the variable, using an optional custom delimiter.
    Prepend,
}

impl std::str::FromStr for ActionType {
    type Err = ();

    fn from_str(suffix: &str) -> Result<Self, Self::Err> {
        match suffix {
            "override" => Ok(ActionType::Override),
            "default" => Ok(ActionType::Default),
            "append" => Ok(ActionType::Append),
            "prepend" => Ok(ActionType::Prepend),
            _ => Err(()),
        }
    }
}

/// The list of environment variables that are explicitly excluded from leaking into the final launch process environment.
pub const LAUNCH_ENV_EXCLUDELIST: &[&str] = &[
    "CNB_LAYERS_DIR",
    "CNB_APP_DIR",
    "CNB_PROCESS_TYPE",
    "CNB_PLATFORM_API",
    "CNB_DEPRECATION_MODE",
];

/// Errors that can occur when processing the launch environment.
#[derive(Debug, thiserror::Error)]
pub enum LaunchEnvError {
    /// Failed to canonicalize a layer directory path.
    #[error("Canonicalize layer dir '{path}': {error}")]
    Canonicalize {
        /// The path that failed canonicalization.
        path: String,
        /// The underlying I/O error.
        error: std::io::Error,
    },
    /// Failed to list the contents of an environment directory.
    #[error("List env dir '{path}': {error}")]
    ListDir {
        /// The directory path that failed to list.
        path: String,
        /// The underlying I/O error.
        error: std::io::Error,
    },
    /// Failed to read the contents of an environment variable file.
    #[error("Read env file '{path}': {error}")]
    ReadFile {
        /// The file path that failed to read.
        path: String,
        /// The underlying I/O error.
        error: std::io::Error,
    },
}

/// Encapsulates the execution environment variables and layer-sourcing modifications for the launch process.
pub struct LaunchEnv {
    vars: HashMap<String, String>,
    root_dir_map: HashMap<String, String>,
}

impl LaunchEnv {
    /// Creates a new `LaunchEnv` populated from the host environment variables.
    /// Excludes variables defined in `LAUNCH_ENV_EXCLUDELIST` and sanitizes the `PATH`
    /// by stripping out the `process_dir` and `lifecycle_dir` to prevent runtime pollution.
    pub fn new<I>(environ: I, process_dir: &str, lifecycle_dir: &str) -> Self
    where
        I: IntoIterator<Item = (String, String)>,
    {
        let mut vars = HashMap::new();

        for (k, v) in environ {
            if LAUNCH_ENV_EXCLUDELIST.contains(&k.as_str()) {
                continue;
            }
            vars.insert(k, v);
        }

        // Sanitize PATH
        if let Some(path_val) = vars.get("PATH").cloned() {
            let parts = std::env::split_paths(&path_val);
            let mut stripped = Vec::new();
            let proc_path = Path::new(process_dir);
            let lc_path = Path::new(lifecycle_dir);
            for part in parts {
                if part == proc_path || part == lc_path {
                    continue;
                }
                stripped.push(part);
            }
            if let Ok(new_path) = std::env::join_paths(stripped) {
                vars.insert("PATH".to_string(), new_path.to_string_lossy().into_owned());
            }
        }

        let mut root_dir_map = HashMap::new();
        root_dir_map.insert("bin".to_string(), "PATH".to_string());
        root_dir_map.insert("lib".to_string(), "LD_LIBRARY_PATH".to_string());

        LaunchEnv { vars, root_dir_map }
    }

    /// Sets an environment variable value directly.
    pub fn set(&mut self, k: &str, v: &str) {
        self.vars.insert(k.to_string(), v.to_string());
    }

    /// Gets an environment variable value.
    pub fn get(&self, k: &str) -> Option<&String> {
        self.vars.get(k)
    }

    /// Appends a root layer path to standard PATH and LD_LIBRARY_PATH variables.
    pub fn add_root_dir<P: AsRef<Path>>(&mut self, layer_dir: P) -> Result<(), LaunchEnvError> {
        let layer_dir = layer_dir.as_ref();
        let abs_dir = fs::canonicalize(layer_dir).map_err(|e| LaunchEnvError::Canonicalize {
            path: layer_dir.to_string_lossy().into_owned(),
            error: e,
        })?;

        for (sub_dir, var_name) in &self.root_dir_map {
            let child_dir = abs_dir.join(sub_dir);
            if child_dir.is_dir() {
                let child_str = child_dir.to_string_lossy().into_owned();
                let current = self.vars.get(var_name).cloned().unwrap_or_default();
                if current.is_empty() {
                    self.vars.insert(var_name.clone(), child_str.clone());
                } else {
                    // Prepend layer path using standard PATH separator
                    let mut paths = vec![PathBuf::from(&child_str)];
                    paths.extend(std::env::split_paths(&current));
                    if let Ok(new_path) = std::env::join_paths(paths) {
                        self.vars
                            .insert(var_name.clone(), new_path.to_string_lossy().into_owned());
                    }
                }
            }
        }
        Ok(())
    }

    /// Processes a directory containing environment files and applies them sequentially.
    pub fn add_env_dir<P: AsRef<Path>>(
        &mut self,
        env_dir: P,
        default_action: ActionType,
    ) -> Result<(), LaunchEnvError> {
        let env_dir = env_dir.as_ref();
        if !env_dir.is_dir() {
            return Ok(());
        }

        let entries = fs::read_dir(env_dir).map_err(|e| LaunchEnvError::ListDir {
            path: env_dir.to_string_lossy().into_owned(),
            error: e,
        })?;

        let mut files: Vec<(std::ffi::OsString, std::fs::DirEntry)> = entries
            .filter_map(|entry_res| {
                let entry = match entry_res {
                    Ok(e) => e,
                    Err(err) => {
                        return Some(Err(LaunchEnvError::ListDir {
                            path: env_dir.to_string_lossy().into_owned(),
                            error: err,
                        }));
                    }
                };
                match fs::metadata(entry.path()) {
                    Ok(meta) if meta.is_dir() => None,
                    Ok(_) => Some(Ok((entry.file_name(), entry))),
                    Err(_) => None,
                }
            })
            .collect::<Result<Vec<_>, LaunchEnvError>>()?;

        files.sort_unstable_by(|a, b| a.0.cmp(&b.0));

        for (file_name_os, file) in files {
            let file_name = file_name_os.to_string_lossy().into_owned();
            let Some((name, action)) = parse_env_file_parts(&file_name, default_action) else {
                continue;
            };
            let file_path = file.path();

            let v = fs::read_to_string(&file_path).map_err(|e| LaunchEnvError::ReadFile {
                path: file_path.to_string_lossy().into_owned(),
                error: e,
            })?;

            // Read custom delimiter if present
            let delim_path = env_dir.join(format!("{}.delim", name));
            let delim = if delim_path.is_file() {
                fs::read_to_string(&delim_path).ok()
            } else {
                None
            };

            if let Some(new_val) = self.evaluate_env_modifier(&name, action, v, delim.as_deref()) {
                self.vars.insert(name, new_val);
            }
        }
        Ok(())
    }

    /// Returns a reference to the internal environment variable map.
    pub fn vars(&self) -> &HashMap<String, String> {
        &self.vars
    }

    /// Evaluates how to modify the environment variable value based on the specified [`ActionType`].
    ///
    /// Returns `Some(String)` with the new value if the action is successful, or `None` if the action
    /// specifies a modification that should be skipped (e.g., `Default` on a non-empty variable).
    fn evaluate_env_modifier(
        &self,
        name: &str,
        action: ActionType,
        v: String,
        delim: Option<&str>,
    ) -> Option<String> {
        match action {
            ActionType::Override => Some(v),
            ActionType::Default => {
                if self.vars.get(name).map(|s| s.is_empty()).unwrap_or(true) {
                    Some(v)
                } else {
                    None
                }
            }
            ActionType::Append => {
                let d = delim.unwrap_or("");
                match self.vars.get(name) {
                    Some(current) if !current.is_empty() => Some(format!("{}{}{}", current, d, v)),
                    _ => Some(v),
                }
            }
            ActionType::Prepend => {
                let d = delim.unwrap_or("");
                match self.vars.get(name) {
                    Some(current) if !current.is_empty() => Some(format!("{}{}{}", v, d, current)),
                    _ => Some(v),
                }
            }
        }
    }
}

/// Parses a filename from an environment directory into the target environment variable name
/// and the corresponding [`ActionType`] modifier suffix.
///
/// File names can optionally end with a dot followed by the action type (e.g. `.override`, `.default`,
/// `.append`, `.prepend`). If no suffix is present, the specified `default_action` is returned.
/// Files ending with `.delim` are ignored.
fn parse_env_file_parts(
    file_name: &str,
    default_action: ActionType,
) -> Option<(String, ActionType)> {
    let (name, suffix) = match file_name.split_once('.') {
        Some((n, s)) => (n, s),
        None => (file_name, ""),
    };

    // Delimiter files are ignored in the main action loop
    if suffix == "delim" {
        return None;
    }

    let action = match suffix {
        "" => default_action,
        other => other.parse().ok()?,
    };

    Some((name.to_string(), action))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_new_launch_env_purging_and_sanitization() {
        let path_val = if cfg!(windows) {
            "/lifecycle;/process;/usr/bin"
        } else {
            "/lifecycle:/process:/usr/bin"
        };
        let host_env = vec![
            ("PATH".to_string(), path_val.to_string()),
            ("CNB_APP_DIR".to_string(), "/workspace".to_string()),
            ("FOO".to_string(), "bar".to_string()),
        ];
        let env = LaunchEnv::new(host_env, "/process", "/lifecycle");

        assert!(env.get("CNB_APP_DIR").is_none());
        assert_eq!(env.get("FOO").unwrap(), "bar");
        assert_eq!(env.get("PATH").unwrap(), "/usr/bin");
    }

    #[test]
    fn test_add_env_dir_override_and_default() {
        let dir = tempdir().unwrap();
        let dir_path = dir.path();

        // 1. Override suffix
        fs::write(dir_path.join("FOO"), "unsuffixed_val").unwrap();
        fs::write(dir_path.join("BAR.override"), "override_val").unwrap();

        let mut env = LaunchEnv::new(std::iter::empty(), "", "");
        env.set("FOO", "original_foo");
        env.set("BAR", "original_bar");

        env.add_env_dir(dir_path, ActionType::Override).unwrap();

        assert_eq!(env.get("FOO").unwrap(), "unsuffixed_val");
        assert_eq!(env.get("BAR").unwrap(), "override_val");

        // 2. Default suffix
        let dir2 = tempdir().unwrap();
        let dir2_path = dir2.path();
        fs::write(dir2_path.join("FOO.default"), "default_val").unwrap();
        fs::write(dir2_path.join("BAZ.default"), "default_val").unwrap();

        env.add_env_dir(dir2_path, ActionType::Override).unwrap();

        // FOO already exists, so default does not override it
        assert_eq!(env.get("FOO").unwrap(), "unsuffixed_val");
        // BAZ does not exist, so it gets set
        assert_eq!(env.get("BAZ").unwrap(), "default_val");
    }

    #[test]
    fn test_add_env_dir_append_and_prepend() {
        let dir = tempdir().unwrap();
        let dir_path = dir.path();

        fs::write(dir_path.join("PATH.prepend"), "/layer/bin").unwrap();
        fs::write(dir_path.join("VAR.append"), "appendage").unwrap();
        fs::write(dir_path.join("VAR.delim"), "-").unwrap();

        let mut env = LaunchEnv::new(std::iter::empty(), "", "");
        env.set("PATH", "/usr/bin");
        env.set("VAR", "base");

        env.add_env_dir(dir_path, ActionType::Override).unwrap();

        // PATH prepends without separator unless .delim specifies one
        let expected_path = "/layer/bin/usr/bin";
        assert_eq!(env.get("PATH").unwrap(), expected_path);
        // VAR uses custom delimiter "-"
        assert_eq!(env.get("VAR").unwrap(), "base-appendage");
    }

    #[test]
    fn test_parse_env_file_parts() {
        use super::{ActionType, parse_env_file_parts};

        // Valid suffixes
        assert_eq!(
            parse_env_file_parts("MY_VAR.override", ActionType::Default),
            Some(("MY_VAR".to_string(), ActionType::Override))
        );
        assert_eq!(
            parse_env_file_parts("MY_VAR.default", ActionType::Override),
            Some(("MY_VAR".to_string(), ActionType::Default))
        );
        assert_eq!(
            parse_env_file_parts("MY_VAR.append", ActionType::Override),
            Some(("MY_VAR".to_string(), ActionType::Append))
        );
        assert_eq!(
            parse_env_file_parts("MY_VAR.prepend", ActionType::Override),
            Some(("MY_VAR".to_string(), ActionType::Prepend))
        );

        // Unsuffixed file uses the default action
        assert_eq!(
            parse_env_file_parts("MY_VAR", ActionType::Default),
            Some(("MY_VAR".to_string(), ActionType::Default))
        );

        // Multiple periods (spec compliance: split on the first period)
        // Suffix is "name.override" (unknown) -> ignored (returns None)
        assert_eq!(
            parse_env_file_parts("MY_VAR.name.override", ActionType::Default),
            None
        );

        // Delimiter files -> ignored (returns None)
        assert_eq!(
            parse_env_file_parts("MY_VAR.delim", ActionType::Default),
            None
        );

        // Unknown suffix -> ignored (returns None)
        assert_eq!(
            parse_env_file_parts("MY_VAR.invalid_suffix", ActionType::Default),
            None
        );
    }

    #[test]
    fn test_evaluate_env_modifier() {
        use super::{ActionType, LaunchEnv};

        let env = LaunchEnv::new(std::iter::empty(), "", "");

        // 1. Override
        assert_eq!(
            env.evaluate_env_modifier("FOO", ActionType::Override, "val1".to_string(), None),
            Some("val1".to_string())
        );

        // 2. Default
        // When empty -> Some
        assert_eq!(
            env.evaluate_env_modifier("BAR", ActionType::Default, "val1".to_string(), None),
            Some("val1".to_string())
        );

        // When not empty -> None
        let mut env_with_val = LaunchEnv::new(std::iter::empty(), "", "");
        env_with_val.set("BAR", "val1");
        assert_eq!(
            env_with_val.evaluate_env_modifier(
                "BAR",
                ActionType::Default,
                "val2".to_string(),
                None
            ),
            None
        );

        // 3. Append (default no delimiter)
        assert_eq!(
            env_with_val.evaluate_env_modifier("BAR", ActionType::Append, "val2".to_string(), None),
            Some("val1val2".to_string())
        );

        // 4. Append with custom delimiter
        assert_eq!(
            env_with_val.evaluate_env_modifier(
                "BAR",
                ActionType::Append,
                "val3".to_string(),
                Some("-")
            ),
            Some("val1-val3".to_string())
        );

        // 5. Prepend path variable (uses no separator without .delim)
        let mut env_with_path = LaunchEnv::new(std::iter::empty(), "", "");
        env_with_path.set("PATH", "/usr/bin");
        let expected = "/bin/usr/bin";
        assert_eq!(
            env_with_path.evaluate_env_modifier(
                "PATH",
                ActionType::Prepend,
                "/bin".to_string(),
                None
            ),
            Some(expected.to_string())
        );
    }

    #[test]
    #[cfg(windows)]
    fn test_new_launch_env_purging_mixed_slashes_windows() {
        let host_env = vec![(
            "PATH".to_string(),
            r"\lifecycle;C:\process;C:\usr\bin".to_string(),
        )];
        // Mixed slash input: forward slash in process_dir and lifecycle_dir
        let env = LaunchEnv::new(host_env, "C:/process", "/lifecycle");

        assert_eq!(env.get("PATH").unwrap(), r"C:\usr\bin");
    }

    #[test]
    fn test_new_launch_env_purging_trailing_slashes() {
        let path_val = if cfg!(windows) {
            "/lifecycle/;/process/;/usr/bin"
        } else {
            "/lifecycle/:/process/:/usr/bin"
        };
        let host_env = vec![("PATH".to_string(), path_val.to_string())];
        let env = LaunchEnv::new(host_env, "/process", "/lifecycle");

        assert_eq!(env.get("PATH").unwrap(), "/usr/bin");
    }
}

#[cfg(test)]
mod ported_rust_tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    // ----- pass: override replaces value, ignores any .delim sidecar -----
    #[test]
    fn override_replaces_value_and_ignores_delim() {
        let dir = tempdir().unwrap();
        let dir_path = dir.path();
        fs::write(dir_path.join("FOO.override"), "new").unwrap();
        fs::write(dir_path.join("FOO.delim"), "::").unwrap();
        let mut env = LaunchEnv::new(std::iter::empty(), "", "");
        env.set("FOO", "old");
        env.add_env_dir(dir_path, ActionType::Override).unwrap();
        assert_eq!(env.get("FOO").map(String::as_str), Some("new"));
    }

    // ----- pass: default only writes when current is empty/unset -----
    #[test]
    fn default_only_writes_when_empty() {
        let dir = tempdir().unwrap();
        let dir_path = dir.path();
        fs::write(dir_path.join("FOO.default"), "fallback").unwrap();
        fs::write(dir_path.join("BAR.default"), "set-bar").unwrap();
        let mut env = LaunchEnv::new(std::iter::empty(), "", "");
        env.set("FOO", "preset");
        env.add_env_dir(dir_path, ActionType::Override).unwrap();
        assert_eq!(env.get("FOO").map(String::as_str), Some("preset"));
        assert_eq!(env.get("BAR").map(String::as_str), Some("set-bar"));
    }

    // ----- pass: append uses explicit .delim when present -----
    #[test]
    fn append_uses_delim_when_present() {
        let dir = tempdir().unwrap();
        let dir_path = dir.path();
        fs::write(dir_path.join("PATH.append"), "/tail").unwrap();
        fs::write(dir_path.join("PATH.delim"), ":").unwrap();
        let mut env = LaunchEnv::new(std::iter::empty(), "", "");
        env.set("PATH", "/head");
        env.add_env_dir(dir_path, ActionType::Override).unwrap();
        assert_eq!(env.get("PATH").map(String::as_str), Some("/head:/tail"));
    }

    // ----- pass: append with no .delim concatenates raw (empty default delim) -----
    #[test]
    fn append_without_delim_concatenates_raw() {
        let dir = tempdir().unwrap();
        let dir_path = dir.path();
        fs::write(dir_path.join("FOO.append"), "Y").unwrap();
        let mut env = LaunchEnv::new(std::iter::empty(), "", "");
        env.set("FOO", "X");
        env.add_env_dir(dir_path, ActionType::Override).unwrap();
        assert_eq!(env.get("FOO").map(String::as_str), Some("XY"));
    }

    // ----- pass: prepend with no .delim concatenates raw (empty default delim) -----
    #[test]
    fn prepend_without_delim_concatenates_raw() {
        let dir = tempdir().unwrap();
        let dir_path = dir.path();
        fs::write(dir_path.join("FOO.prepend"), "X").unwrap();
        let mut env = LaunchEnv::new(std::iter::empty(), "", "");
        env.set("FOO", "Y");
        env.add_env_dir(dir_path, ActionType::Override).unwrap();
        assert_eq!(env.get("FOO").map(String::as_str), Some("XY"));
    }

    // ----- pass: prepend uses explicit .delim when present -----
    #[test]
    fn prepend_uses_delim_when_present() {
        let dir = tempdir().unwrap();
        let dir_path = dir.path();
        fs::write(dir_path.join("PATH.prepend"), "/new").unwrap();
        fs::write(dir_path.join("PATH.delim"), ":").unwrap();
        let mut env = LaunchEnv::new(std::iter::empty(), "", "");
        env.set("PATH", "/old");
        env.add_env_dir(dir_path, ActionType::Override).unwrap();
        assert_eq!(env.get("PATH").map(String::as_str), Some("/new:/old"));
    }

    // ----- pass: a file with an unrecognized suffix is silently skipped -----
    #[test]
    fn unknown_suffix_is_silently_skipped() {
        let dir = tempdir().unwrap();
        let dir_path = dir.path();
        fs::write(dir_path.join("FOO.weirdsuffix"), "v").unwrap();
        let mut env = LaunchEnv::new(std::iter::empty(), "", "");
        env.set("FOO", "keep");
        env.add_env_dir(dir_path, ActionType::Override).unwrap();
        assert_eq!(env.get("FOO").map(String::as_str), Some("keep"));
    }

    // ----- pass: a `.delim` sidecar file is not itself treated as a variable -----
    #[test]
    fn delim_sidecar_not_treated_as_var() {
        // parse_env_file_parts ignores `.delim` files entirely.
        assert_eq!(
            parse_env_file_parts("MY_VAR.delim", ActionType::Override),
            None
        );
        // And applying a dir that only holds a `.delim` file changes nothing.
        let dir = tempdir().unwrap();
        let dir_path = dir.path();
        fs::write(dir_path.join("FOO.delim"), ":").unwrap();
        let mut env = LaunchEnv::new(std::iter::empty(), "", "");
        env.add_env_dir(dir_path, ActionType::Override).unwrap();
        assert_eq!(env.get("FOO"), None);
    }

    // ----- pass: filename is split on the FIRST '.'; multi-dot suffix is unknown -> None -----
    #[test]
    fn split_on_first_dot_suffix() {
        // "MY_VAR.name.override" splits to name="MY_VAR", suffix="name.override" (unknown) -> None.
        assert_eq!(
            parse_env_file_parts("MY_VAR.name.override", ActionType::Default),
            None
        );
        // Single recognized suffix parses normally.
        assert_eq!(
            parse_env_file_parts("MY_VAR.override", ActionType::Default),
            Some(("MY_VAR".to_string(), ActionType::Override))
        );
    }

    // ----- pass: excludelist keys are dropped from the launch env -----
    #[test]
    fn vars_excludelist_drops_keys() {
        let pairs = vec![
            ("KEEP".to_string(), "1".to_string()),
            ("CNB_APP_DIR".to_string(), "/x".to_string()),
            ("CNB_PLATFORM_API".to_string(), "0.10".to_string()),
        ];
        let env = LaunchEnv::new(pairs, "", "");
        assert_eq!(env.get("KEEP").map(String::as_str), Some("1"));
        assert_eq!(env.get("CNB_APP_DIR"), None);
        assert_eq!(env.get("CNB_PLATFORM_API"), None);
    }

    // ----- pass: basic (key,value) pairs survive construction; '=' in value preserved -----
    #[test]
    fn vars_from_pairs_basic() {
        let pairs = vec![
            ("FOO".to_string(), "bar".to_string()),
            ("BAZ".to_string(), "qux=etc".to_string()),
        ];
        let env = LaunchEnv::new(pairs, "", "");
        assert_eq!(env.get("FOO").map(String::as_str), Some("bar"));
        assert_eq!(env.get("BAZ").map(String::as_str), Some("qux=etc"));
    }

    // ----- pass: process_dir/lifecycle_dir are stripped from PATH at construction -----
    #[test]
    fn new_launch_env_seeds_path_dirs() {
        let path_val = if cfg!(windows) {
            "/lifecycle;/process;/usr/bin"
        } else {
            "/lifecycle:/process:/usr/bin"
        };
        let pairs = vec![("PATH".to_string(), path_val.to_string())];
        let env = LaunchEnv::new(pairs, "/process", "/lifecycle");
        assert_eq!(env.get("PATH").map(String::as_str), Some("/usr/bin"));
    }

    // ----- pass: subdirectories within an env dir are skipped, not applied -----
    #[test]
    fn add_env_dir_skips_subdirectories() {
        let dir = tempdir().unwrap();
        let dir_path = dir.path();
        // A directory named like an override file must be ignored.
        fs::create_dir_all(dir_path.join("FOO.override")).unwrap();
        // A regular file alongside it is still applied.
        fs::write(dir_path.join("BAR.override"), "bar-val").unwrap();
        let mut env = LaunchEnv::new(std::iter::empty(), "", "");
        env.set("FOO", "original");
        env.add_env_dir(dir_path, ActionType::Override).unwrap();
        assert_eq!(env.get("FOO").map(String::as_str), Some("original"));
        assert_eq!(env.get("BAR").map(String::as_str), Some("bar-val"));
    }

    // ----- pass: the launch default action is Override (unsuffixed file uses it) -----
    #[test]
    fn default_action_is_override() {
        // No `default_action_type()` fn exists in launcher-rs; pin the same
        // Go-correct Override default via the unsuffixed-uses-default seam.
        assert_eq!(
            parse_env_file_parts("FOO", ActionType::Override),
            Some(("FOO".to_string(), ActionType::Override))
        );
    }

    // ----- pass: prepend followed by prepend accumulates left-to-right -----
    #[test]
    fn prepend_then_prepend_accumulates() {
        let mut env = LaunchEnv::new(std::iter::empty(), "", "");
        env.set("PATH", "/base");

        let dir1 = tempdir().unwrap();
        fs::write(dir1.path().join("PATH.prepend"), "/first").unwrap();
        fs::write(dir1.path().join("PATH.delim"), ":").unwrap();
        env.add_env_dir(dir1.path(), ActionType::Override).unwrap();
        assert_eq!(env.get("PATH").map(String::as_str), Some("/first:/base"));

        let dir2 = tempdir().unwrap();
        fs::write(dir2.path().join("PATH.prepend"), "/second").unwrap();
        fs::write(dir2.path().join("PATH.delim"), ":").unwrap();
        env.add_env_dir(dir2.path(), ActionType::Override).unwrap();
        assert_eq!(
            env.get("PATH").map(String::as_str),
            Some("/second:/first:/base")
        );
    }

    // ----- pass: override then a later default does NOT clobber the override -----
    #[test]
    fn override_then_default_keeps_override() {
        let mut env = LaunchEnv::new(std::iter::empty(), "", "");

        let dir1 = tempdir().unwrap();
        fs::write(dir1.path().join("FOO.override"), "overridden").unwrap();
        env.add_env_dir(dir1.path(), ActionType::Override).unwrap();
        assert_eq!(env.get("FOO").map(String::as_str), Some("overridden"));

        let dir2 = tempdir().unwrap();
        fs::write(dir2.path().join("FOO.default"), "fallback").unwrap();
        env.add_env_dir(dir2.path(), ActionType::Override).unwrap();
        // Default skips because FOO is already non-empty.
        assert_eq!(env.get("FOO").map(String::as_str), Some("overridden"));
    }

    // ----- pass: an empty env dir is a no-op -----
    #[test]
    fn empty_env_dir_is_noop() {
        let dir = tempdir().unwrap();
        let mut env = LaunchEnv::new(std::iter::empty(), "", "");
        env.add_env_dir(dir.path(), ActionType::Override).unwrap();
        assert!(env.vars().is_empty());
    }

    // GAP: launcher-rs strips trailing-slash PATH entries ('/proc/' purged) via
    // split_paths/join_paths normalization, but Go strips by EXACT string match so
    // '/proc/' (!= '/proc') is KEPT. Asserting the Go-correct kept value FAILS
    // against launcher-rs (cf. its own test_new_launch_env_purging_trailing_slashes).
    #[test]
    fn strip_path_removes_exact_matches() {
        let pairs = vec![("PATH".to_string(), "/a:/proc/:/b".to_string())];
        let env = LaunchEnv::new(pairs, "/proc", "/life");
        // Go-correct: trailing-slash entry is not an exact match -> retained.
        assert_eq!(env.get("PATH").map(String::as_str), Some("/a:/proc/:/b"));
    }

    // GAP: launcher-rs add_root_dir calls fs::canonicalize(layer_dir) first, which
    // on macOS rewrites the layer path (e.g. /var -> /private/var symlink resolution),
    // whereas Go uses filepath.Abs (no symlink resolution). Asserting the Go-correct
    // un-canonicalized layer/bin path FAILS against launcher-rs's canonicalized output.
    #[test]
    fn add_root_dir_prepends_existing_subdirs() {
        let layer = tempdir().unwrap();
        let layer_path = layer.path();
        fs::create_dir_all(layer_path.join("bin")).unwrap();
        fs::create_dir_all(layer_path.join("lib")).unwrap();
        let mut env = LaunchEnv::new(std::iter::empty(), "", "");
        env.set("PATH", "/usr/bin");
        env.add_root_dir(layer_path).unwrap();
        // Go-correct: raw (Abs, non-canonicalized) layer/bin prefix.
        let bin = layer_path.join("bin").to_string_lossy().into_owned();
        let lib = layer_path.join("lib").to_string_lossy().into_owned();
        assert_eq!(
            env.get("PATH").map(String::as_str),
            Some(format!("{}:/usr/bin", bin).as_str())
        );
        assert_eq!(env.get("LD_LIBRARY_PATH").map(String::as_str), Some(lib.as_str()));
    }

    // GAP: Go AddRootDir sets the missing subdir's var to the empty string ("");
    // launcher-rs never inserts the key when the child dir is absent, so get()
    // returns None. Asserting the Go-correct Some("") FAILS (it is None).
    #[test]
    fn add_root_dir_skips_missing_subdirs() {
        let layer = tempdir().unwrap();
        let layer_path = layer.path();
        fs::create_dir_all(layer_path.join("bin")).unwrap();
        let mut env = LaunchEnv::new(std::iter::empty(), "", "");
        env.add_root_dir(layer_path).unwrap();
        assert_eq!(env.get("PATH").map(|s| !s.is_empty()), Some(true));
        // Go-correct: missing lib -> LD_LIBRARY_PATH is set to "".
        assert_eq!(env.get("LD_LIBRARY_PATH").map(String::as_str), Some(""));
    }

    // GAP: Go AddRootDir uses filepath.Abs (a pure path op, never stats) so a missing
    // layer dir is not an error -- subdir Stat just misses and is skipped, returning nil.
    // launcher-rs canonicalizes first (fs::canonicalize), which errors on a missing dir.
    // Asserting the Go-correct Ok FAILS (launcher-rs returns Err).
    #[test]
    fn add_root_dir_missing_layer_errors_not_noop() {
        let base = tempdir().unwrap();
        let missing = base.path().join("no-such-layer");
        let mut env = LaunchEnv::new(std::iter::empty(), "", "");
        let r = env.add_root_dir(&missing);
        // Go-correct: missing layer is a no-op, not an error.
        assert!(r.is_ok());
    }

    // GAP: same canonicalize-vs-Abs divergence as add_root_dir_prepends_existing_subdirs.
    // The composed PATH assertion uses the Go-correct un-canonicalized layer/bin prefix,
    // which FAILS against launcher-rs's canonicalized add_root_dir output. (The FOO=bar
    // and vars() membership assertions are Go-correct and pass.)
    #[test]
    fn env_wrapper_round_trip() {
        let layer = tempdir().unwrap();
        let layer_path = layer.path();
        fs::create_dir_all(layer_path.join("bin")).unwrap();
        let env_dir = layer_path.join("env");
        fs::create_dir_all(&env_dir).unwrap();
        fs::write(env_dir.join("FOO.override"), "bar").unwrap();

        let mut env = LaunchEnv::new(std::iter::empty(), "", "");
        env.set("PATH", "/usr/bin");
        env.add_root_dir(layer_path).unwrap();
        env.add_env_dir(&env_dir, ActionType::Override).unwrap();

        assert_eq!(env.get("FOO").map(String::as_str), Some("bar"));
        let bin = layer_path.join("bin").to_string_lossy().into_owned();
        assert_eq!(
            env.get("PATH").map(String::as_str),
            Some(format!("{}:/usr/bin", bin).as_str())
        );
        assert!(env.vars().iter().any(|(k, v)| k == "FOO" && v == "bar"));
    }

    // GAP: Go's empty-suffix PrependPath action uses os.PathListSeparator (':') as the
    // default delim for PATH-like vars: '/bin' prepended to '/usr/bin' -> '/bin:/usr/bin'.
    // launcher-rs has NO PrependPath action; generic Prepend defaults the delim to "" so it
    // returns '/bin/usr/bin' (raw concat). Asserting the Go-correct ':' value FAILS
    // (cf. launcher-rs's own test_evaluate_env_modifier which asserts '/bin/usr/bin').
    #[test]
    fn evaluate_env_modifier_prepend_path_no_delim_raw_concat() {
        let mut env = LaunchEnv::new(std::iter::empty(), "", "");
        env.set("PATH", "/usr/bin");
        let got =
            env.evaluate_env_modifier("PATH", ActionType::Prepend, "/bin".to_string(), None);
        // Go PrependPath-correct: ':' default separator for PATH.
        assert_eq!(got, Some("/bin:/usr/bin".to_string()));
    }
}
