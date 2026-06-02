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

/// The list of environment variables that are explicitly excluded from leaking into the final launch process environment.
pub const LAUNCH_ENV_EXCLUDELIST: &[&str] = &[
    "CNB_LAYERS_DIR",
    "CNB_APP_DIR",
    "CNB_PROCESS_TYPE",
    "CNB_PLATFORM_API",
    "CNB_DEPRECATION_MODE",
];

/// Encapsulates the execution environment variables and layer-sourcing modifications for the launch process.
pub struct LaunchEnv {
    vars: HashMap<String, String>,
    root_dir_map: HashMap<String, String>,
}

impl LaunchEnv {
    /// Creates a new `LaunchEnv` populated from the host environment variables.
    /// Excludes variables defined in `LAUNCH_ENV_EXCLUDELIST` and sanitizes the `PATH`
    /// by stripping out the `process_dir` and `lifecycle_dir` to prevent runtime pollution.
    pub fn new(environ: &[(String, String)], process_dir: &str, lifecycle_dir: &str) -> Self {
        let mut vars = HashMap::new();

        for (k, v) in environ {
            if LAUNCH_ENV_EXCLUDELIST.contains(&k.as_str()) {
                continue;
            }
            vars.insert(k.clone(), v.clone());
        }

        // Sanitize PATH
        if let Some(path_val) = vars.get("PATH").cloned() {
            let parts = std::env::split_paths(&path_val);
            let mut stripped = Vec::new();
            for part in parts {
                if part.to_str() == Some(process_dir) || part.to_str() == Some(lifecycle_dir) {
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
    pub fn add_root_dir(&mut self, layer_dir: &str) -> Result<(), String> {
        let abs_dir = fs::canonicalize(layer_dir)
            .map_err(|e| format!("Canonicalize layer dir '{}': {}", layer_dir, e))?;

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
    pub fn add_env_dir(&mut self, env_dir: &str, default_action: ActionType) -> Result<(), String> {
        let path = Path::new(env_dir);
        if !path.is_dir() {
            return Ok(());
        }

        let entries =
            fs::read_dir(path).map_err(|e| format!("List env dir '{}': {}", env_dir, e))?;

        let mut files: Vec<_> = entries
            .filter_map(|entry_res| {
                let entry = match entry_res {
                    Ok(e) => e,
                    Err(err) => return Some(Err(err.to_string())),
                };
                match fs::metadata(entry.path()) {
                    Ok(meta) if meta.is_dir() => None,
                    Ok(_) => Some(Ok(entry)),
                    Err(_) => None,
                }
            })
            .collect::<Result<Vec<_>, String>>()?;
        files.sort_by_key(|f| f.file_name());

        for file in files {
            let file_name = file.file_name().to_string_lossy().into_owned();
            let Some((name, action)) = parse_env_file_parts(&file_name, default_action) else {
                continue;
            };
            let file_path = file.path();

            let v = fs::read_to_string(&file_path)
                .map_err(|e| format!("Read env file '{}': {}", file_path.display(), e))?;

            // Read custom delimiter if present
            let delim_path = Path::new(env_dir).join(format!("{}.delim", name));
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

    fn evaluate_env_modifier(&self, name: &str, action: ActionType, v: String, delim: Option<&str>) -> Option<String> {
        let current = self.vars.get(name).cloned().unwrap_or_default();
        match action {
            ActionType::Override => Some(v),
            ActionType::Default => {
                if current.is_empty() {
                    Some(v)
                } else {
                    None
                }
            }
            ActionType::Append => {
                let d = delim.unwrap_or("");
                let new_val = if current.is_empty() {
                    v
                } else {
                    format!("{}{}{}", current, d, v)
                };
                Some(new_val)
            }
            ActionType::Prepend => {
                let d = delim.unwrap_or_else(|| {
                    if name == "PATH" || name == "LD_LIBRARY_PATH" {
                        if cfg!(windows) { ";" } else { ":" }
                    } else {
                        ""
                    }
                });
                let new_val = if current.is_empty() {
                    v
                } else {
                    format!("{}{}{}", v, d, current)
                };
                Some(new_val)
            }
        }
    }
}

fn parse_env_file_parts(file_name: &str, default_action: ActionType) -> Option<(String, ActionType)> {
    let parts: Vec<&str> = file_name.splitn(2, '.').collect();
    let name = parts[0].to_string();
    let suffix = if parts.len() > 1 { parts[1] } else { "" };

    // Delimiter files are ignored in the main action loop
    if suffix == "delim" {
        return None;
    }

    let action = match suffix {
        "override" => ActionType::Override,
        "default" => ActionType::Default,
        "append" => ActionType::Append,
        "prepend" => ActionType::Prepend,
        "" => default_action,
        _ => return None, // Ignore files with unknown suffixes
    };

    Some((name, action))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_new_launch_env_purging_and_sanitization() {
        let host_env = vec![
            (
                "PATH".to_string(),
                "/lifecycle:/process:/usr/bin".to_string(),
            ),
            ("CNB_APP_DIR".to_string(), "/workspace".to_string()),
            ("FOO".to_string(), "bar".to_string()),
        ];
        let env = LaunchEnv::new(&host_env, "/process", "/lifecycle");

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

        let mut env = LaunchEnv::new(&[], "", "");
        env.set("FOO", "original_foo");
        env.set("BAR", "original_bar");

        env.add_env_dir(&dir_path.to_string_lossy(), ActionType::Override)
            .unwrap();

        assert_eq!(env.get("FOO").unwrap(), "unsuffixed_val");
        assert_eq!(env.get("BAR").unwrap(), "override_val");

        // 2. Default suffix
        let dir2 = tempdir().unwrap();
        let dir2_path = dir2.path();
        fs::write(dir2_path.join("FOO.default"), "default_val").unwrap();
        fs::write(dir2_path.join("BAZ.default"), "default_val").unwrap();

        env.add_env_dir(&dir2_path.to_string_lossy(), ActionType::Override)
            .unwrap();

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

        let mut env = LaunchEnv::new(&[], "", "");
        env.set("PATH", "/usr/bin");
        env.set("VAR", "base");

        env.add_env_dir(&dir_path.to_string_lossy(), ActionType::Override)
            .unwrap();

        // PATH uses default separator (":" on unix)
        assert_eq!(env.get("PATH").unwrap(), "/layer/bin:/usr/bin");
        // VAR uses custom delimiter "-"
        assert_eq!(env.get("VAR").unwrap(), "base-appendage");
    }

    #[test]
    fn test_parse_env_file_parts() {
        use super::{parse_env_file_parts, ActionType};

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
        use super::{LaunchEnv, ActionType};

        let env = LaunchEnv::new(&[], "", "");

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
        let mut env_with_val = LaunchEnv::new(&[], "", "");
        env_with_val.set("BAR", "val1");
        assert_eq!(
            env_with_val.evaluate_env_modifier("BAR", ActionType::Default, "val2".to_string(), None),
            None
        );

        // 3. Append (default no delimiter)
        assert_eq!(
            env_with_val.evaluate_env_modifier("BAR", ActionType::Append, "val2".to_string(), None),
            Some("val1val2".to_string())
        );

        // 4. Append with custom delimiter
        assert_eq!(
            env_with_val.evaluate_env_modifier("BAR", ActionType::Append, "val3".to_string(), Some("-")),
            Some("val1-val3".to_string())
        );

        // 5. Prepend path variable (uses default separator)
        let mut env_with_path = LaunchEnv::new(&[], "", "");
        env_with_path.set("PATH", "/usr/bin");
        let expected = if cfg!(windows) { "/bin;/usr/bin" } else { "/bin:/usr/bin" };
        assert_eq!(
            env_with_path.evaluate_env_modifier("PATH", ActionType::Prepend, "/bin".to_string(), None),
            Some(expected.to_string())
        );
    }
}
