//! Primitives for assembling the launch-time environment per the Cloud Native Buildpacks specification.
//!
//! Provides building blocks for sanitizing the host environment, contributing a layer's implicit
//! path entries, and applying the file-based modifications declared in layer env directories.

use std::collections::HashMap;
use std::env::{join_paths, split_paths};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::iter;
use std::path::{Path, PathBuf};

/// Builds the launch environment from a host environment, dropping CNB internal env vars and
/// stripping the given paths from the `PATH` environment variable.
pub(crate) fn sanitize_env_vars_for_launch<I>(
    env_vars: I,
    excluded_paths: &[PathBuf],
) -> HashMap<OsString, OsString>
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    let mut filtered_env_vars = env_vars
        .into_iter()
        .filter(|(env_var_name, _)| {
            LAUNCH_ENV_EXCLUDED_ENV_VARS
                .iter()
                .map(OsStr::new)
                .all(|excluded_env_var_name| env_var_name != excluded_env_var_name)
        })
        .collect::<HashMap<_, _>>();

    if let Some(path_value) = filtered_env_vars.get_mut(OsStr::new("PATH")) {
        *path_value =
            join_paths(split_paths(path_value).filter(|path| !excluded_paths.contains(path)))
                .expect("split_paths output should never contain the platform path separator")
    }

    filtered_env_vars
}

/// Contributes a single layer's paths to the launch-phase path variables
/// listed in the CNB Buildpack spec under "Layer Paths" (`bin/` to `PATH`, `lib/` to `LD_LIBRARY_PATH`).
/// Build-only mappings (`LIBRARY_PATH`, `CPATH`, `PKG_CONFIG_PATH`) are intentionally omitted.
///
/// Prepends to the path list. To get spec-compliant ordering (buildpack groups reversed,
/// layers within a buildpack ascending alphabetically), call this with buildpacks in forward
/// order and layers within each buildpack in reverse-alphabetical order.
pub(crate) fn add_implicit_layer_dir_paths(
    env: &mut HashMap<OsString, OsString>,
    layer_dir: impl AsRef<Path>,
) -> std::io::Result<()> {
    let layer_dir = fs::canonicalize(layer_dir.as_ref())?;

    for (dir_name, env_var_name) in IMPLICIT_LAYER_DIR_PATHS {
        let dir = layer_dir.join(dir_name);

        if !dir.is_dir() {
            continue;
        }

        env.entry(OsString::from(env_var_name))
            .and_modify(|value| {
                *value = join_paths(iter::once(dir.clone()).chain(split_paths(value)))
                    .expect("split_paths output should never contain the platform path separator")
            })
            .or_insert_with(|| dir.into_os_string());
    }

    Ok(())
}

/// Applies the file-based environment-variable modifications from a single env directory, per the
/// CNB Buildpack spec's "Environment Variable Modification Rules".
pub(crate) fn apply_env_dir_modifications(
    env: &mut HashMap<OsString, OsString>,
    env_dir: impl AsRef<Path>,
) -> std::io::Result<()> {
    let env_dir = env_dir.as_ref();

    let mut filtered_dir_entries = fs::read_dir(env_dir)?
        .filter_map(|dir_entry| {
            dir_entry
                .and_then(|dir_entry| {
                    dir_entry
                        .metadata()
                        .map(|metadata| (!metadata.is_dir()).then_some(dir_entry))
                })
                .transpose()
        })
        .collect::<Result<Vec<_>, _>>()?;

    filtered_dir_entries.sort_by_key(|dir_entry| dir_entry.file_name());

    for dir_entry in filtered_dir_entries {
        let file_name = dir_entry.file_name().to_string_lossy().to_string();
        let (file_name_prefix, file_name_suffix) = file_name
            .split_once('.')
            .map(|(prefix, suffix)| (prefix, Some(suffix)))
            .unwrap_or((&file_name, None));

        let env_var = OsString::from(file_name_prefix);
        let env_var_file_contents = read_env_file(&dir_entry.path())?;

        match file_name_suffix {
            None | Some("override") => {
                env.insert(env_var, env_var_file_contents);
            }
            Some("default") => {
                env.entry(env_var).or_insert(env_var_file_contents);
            }
            Some("append" | "prepend") => {
                // Collapses the unset and empty cases to a single empty value. The CNB spec
                // treats both the same by omitting the delimiter.
                let existing_value = env.get(&env_var).cloned().unwrap_or_default();

                let result = if existing_value.is_empty() {
                    env_var_file_contents
                } else {
                    let delim_file_path = env_dir.join(format!("{file_name_prefix}.delim"));
                    let delimiter = delim_file_path
                        .is_file()
                        .then_some(read_env_file(&delim_file_path))
                        .transpose()?
                        .unwrap_or_default();

                    let mut result = OsString::new();
                    match file_name_suffix {
                        Some("append") => {
                            result.push(existing_value);
                            result.push(delimiter);
                            result.push(env_var_file_contents);
                        }
                        Some("prepend") => {
                            result.push(env_var_file_contents);
                            result.push(delimiter);
                            result.push(existing_value);
                        }
                        _ => unreachable!(
                            "outer match guarantees file_name_suffix is Some(\"append\") or Some(\"prepend\"), got {file_name_suffix:?}"
                        ),
                    }
                    result
                };

                env.insert(env_var, result);
            }
            // Delimiter files do nothing on their own and are skipped. See "append" and "prepend"
            // suffixes.
            Some("delim") => continue,
            // Unknown suffixes are silently skipped.
            Some(_) => continue,
        };
    }

    Ok(())
}

#[cfg(unix)]
fn read_env_file(path: &Path) -> std::io::Result<OsString> {
    use std::os::unix::ffi::OsStringExt;
    fs::read(path).map(OsString::from_vec)
}

#[cfg(windows)]
fn read_env_file(path: &Path) -> std::io::Result<OsString> {
    // Windows env vars are UTF-16, and OsString on Windows stores WTF-8 (https://wtf-8.codeberg.page/),
    // so any bytes we accept must be decodable as UTF-something. The CNB spec doesn't define an
    // encoding for env files, but UTF-8 is the only sane assumption we can make here.
    let bytes = fs::read(path)?;
    String::from_utf8(bytes)
        .map(OsString::from)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

/// The list of environment variables that are explicitly excluded from leaking into the final launch process environment.
const LAUNCH_ENV_EXCLUDED_ENV_VARS: &[&str] = &[
    "CNB_LAYERS_DIR",
    "CNB_APP_DIR",
    "CNB_PROCESS_TYPE",
    "CNB_PLATFORM_API",
    "CNB_DEPRECATION_MODE",
];

const IMPLICIT_LAYER_DIR_PATHS: &[(&str, &str)] = &[("bin", "PATH"), ("lib", "LD_LIBRARY_PATH")];

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use tempfile::tempdir;

    #[test]
    fn test_new_launch_env_purging_and_sanitization() {
        let path_val = if cfg!(windows) {
            "/lifecycle;/process;/usr/bin"
        } else {
            "/lifecycle:/process:/usr/bin"
        };

        let host_env = vec![
            (OsString::from("PATH"), OsString::from(path_val)),
            (OsString::from("CNB_APP_DIR"), OsString::from("/workspace")),
            (OsString::from("FOO"), OsString::from("bar")),
        ];

        let env = sanitize_env_vars_for_launch(
            host_env,
            &[PathBuf::from("/process"), PathBuf::from("/lifecycle")],
        );

        assert_eq!(env.get(OsStr::new("CNB_APP_DIR")), None);
        assert_eq!(env.get(OsStr::new("FOO")), Some(&OsString::from("bar")));
        assert_eq!(
            env.get(OsStr::new("PATH")),
            Some(&OsString::from("/usr/bin"))
        );
    }

    #[test]
    fn test_add_env_dir_override_and_default() {
        let dir = tempdir().unwrap();
        let dir_path = dir.path();

        // 1. Override suffix
        fs::write(dir_path.join("FOO"), "unsuffixed_val").unwrap();
        fs::write(dir_path.join("BAR.override"), "override_val").unwrap();

        let mut env =
            sanitize_env_vars_for_launch(iter::empty(), &["/process".into(), "/lifecycle".into()]);
        env.insert(OsString::from("FOO"), OsString::from("original_foo"));
        env.insert(OsString::from("BAR"), OsString::from("original_bar"));

        apply_env_dir_modifications(&mut env, dir_path).unwrap();

        assert_eq!(env.get(OsStr::new("FOO")).unwrap(), "unsuffixed_val");
        assert_eq!(env.get(OsStr::new("BAR")).unwrap(), "override_val");

        // 2. Default suffix
        let dir2 = tempdir().unwrap();
        let dir2_path = dir2.path();
        fs::write(dir2_path.join("FOO.default"), "default_val").unwrap();
        fs::write(dir2_path.join("BAZ.default"), "default_val").unwrap();

        apply_env_dir_modifications(&mut env, dir2_path).unwrap();

        // FOO already exists, so default does not override it
        assert_eq!(env.get(OsStr::new("FOO")).unwrap(), "unsuffixed_val");
        // BAZ does not exist, so it gets set
        assert_eq!(env.get(OsStr::new("BAZ")).unwrap(), "default_val");
    }

    #[test]
    fn test_add_env_dir_append_and_prepend() {
        let dir = tempdir().unwrap();
        let dir_path = dir.path();

        fs::write(dir_path.join("PATH.prepend"), "/layer/bin").unwrap();
        fs::write(dir_path.join("VAR.append"), "appendage").unwrap();
        fs::write(dir_path.join("VAR.delim"), "-").unwrap();

        let mut env = sanitize_env_vars_for_launch(iter::empty(), &[]);
        env.insert(OsString::from("PATH"), OsString::from("/usr/bin"));
        env.insert(OsString::from("VAR"), OsString::from("base"));

        apply_env_dir_modifications(&mut env, dir_path).unwrap();

        // PATH prepends without separator unless .delim specifies one
        let expected_path = "/layer/bin/usr/bin";
        assert_eq!(env.get(OsStr::new("PATH")).unwrap(), expected_path);
        // VAR uses custom delimiter "-"
        assert_eq!(env.get(OsStr::new("VAR")).unwrap(), "base-appendage");
    }

    #[test]
    fn test_apply_env_dir_override_sets_absent_var() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("FOO.override"), "value").unwrap();

        let mut env = HashMap::new();
        apply_env_dir_modifications(&mut env, dir.path()).unwrap();

        assert_eq!(env.get(OsStr::new("FOO")).unwrap(), "value");
    }

    #[test]
    fn test_apply_env_dir_append_without_delim_uses_empty_separator() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("VAR.append"), "appended-value").unwrap();

        let mut env = HashMap::new();
        env.insert(OsString::from("VAR"), OsString::from("base-value"));
        apply_env_dir_modifications(&mut env, dir.path()).unwrap();

        assert_eq!(
            env.get(OsStr::new("VAR")).unwrap(),
            "base-valueappended-value"
        );
    }

    #[test]
    fn test_apply_env_dir_append_on_absent_var_omits_delimiter() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("VAR.append"), "value").unwrap();
        fs::write(dir.path().join("VAR.delim"), ":").unwrap();

        let mut env = HashMap::new();
        apply_env_dir_modifications(&mut env, dir.path()).unwrap();

        assert_eq!(env.get(OsStr::new("VAR")).unwrap(), "value");
    }

    #[test]
    fn test_apply_env_dir_prepend_on_absent_var_omits_delimiter() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("VAR.prepend"), "value").unwrap();
        fs::write(dir.path().join("VAR.delim"), ":").unwrap();

        let mut env = HashMap::new();
        apply_env_dir_modifications(&mut env, dir.path()).unwrap();

        assert_eq!(env.get(OsStr::new("VAR")).unwrap(), "value");
    }

    #[test]
    fn test_apply_env_dir_unknown_suffix_is_ignored() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("FOO.invalid_suffix"), "value").unwrap();

        let mut env = HashMap::new();
        apply_env_dir_modifications(&mut env, dir.path()).unwrap();

        assert!(env.is_empty());
    }

    #[test]
    fn test_apply_env_dir_splits_on_first_dot() {
        // Per CNB spec, the variable name is everything up to the first period.
        // `MY.VAR.override` -> name=`MY`, suffix=`VAR.override` (unknown) -> ignored.
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("MY.VAR.override"), "value").unwrap();

        let mut env = HashMap::new();
        apply_env_dir_modifications(&mut env, dir.path()).unwrap();

        assert!(env.is_empty());
    }

    #[test]
    fn test_apply_env_dir_lone_delim_is_a_noop() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("VAR.delim"), ":").unwrap();

        let mut env = HashMap::new();
        env.insert(OsString::from("VAR"), OsString::from("original"));
        apply_env_dir_modifications(&mut env, dir.path()).unwrap();

        assert_eq!(env.get(OsStr::new("VAR")).unwrap(), "original");
    }

    #[test]
    fn test_apply_env_dir_append_on_empty_var_omits_delimiter() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("VAR.append"), "value").unwrap();
        fs::write(dir.path().join("VAR.delim"), ":").unwrap();

        let mut env = HashMap::new();
        env.insert(OsString::from("VAR"), OsString::new());
        apply_env_dir_modifications(&mut env, dir.path()).unwrap();

        assert_eq!(env.get(OsStr::new("VAR")).unwrap(), "value");
    }

    #[test]
    fn test_apply_env_dir_append_and_prepend_in_same_dir() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("VAR.append"), "appended-value").unwrap();
        fs::write(dir.path().join("VAR.prepend"), "prepended-value").unwrap();
        fs::write(dir.path().join("VAR.delim"), ":").unwrap();

        let mut env = HashMap::new();
        env.insert(OsString::from("VAR"), OsString::from("base-value"));
        apply_env_dir_modifications(&mut env, dir.path()).unwrap();

        assert_eq!(
            env.get(OsStr::new("VAR")).unwrap(),
            "prepended-value:base-value:appended-value"
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_apply_env_dir_broken_symlink_aborts() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        symlink("/nonexistent/target", dir.path().join("FOO")).unwrap();

        let mut env = HashMap::new();
        let result = apply_env_dir_modifications(&mut env, dir.path());

        assert!(result.is_err());
    }

    #[test]
    #[cfg(windows)]
    fn test_new_launch_env_purging_mixed_slashes_windows() {
        let host_env = vec![(
            OsString::from("PATH"),
            OsString::from(r"\lifecycle;C:\process;C:\usr\bin"),
        )];
        // Mixed slash input: forward slash in process_dir and lifecycle_dir
        let env = sanitize_env_vars_for_launch(
            host_env,
            &[PathBuf::from("C:/process"), PathBuf::from("/lifecycle")],
        );

        assert_eq!(env.get(OsStr::new("PATH")).unwrap(), r"C:\usr\bin");
    }

    #[test]
    fn test_new_launch_env_purging_trailing_slashes() {
        let path_val = if cfg!(windows) {
            "/lifecycle/;/process/;/usr/bin"
        } else {
            "/lifecycle/:/process/:/usr/bin"
        };

        let host_env = vec![(OsString::from("PATH"), OsString::from(path_val))];
        let env = sanitize_env_vars_for_launch(host_env, &["/process".into(), "/lifecycle".into()]);

        assert_eq!(env.get(OsStr::new("PATH")).unwrap(), "/usr/bin");
    }
}
