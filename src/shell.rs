//! Shell wrapper and profile sourcing execution.
//!
//! When launching in indirect mode, the target process is wrapped in a shell execution payload.
//! This module handles building and launching the shell wrapper (Bash on Unix, CMD on Windows)
//! and sourcing any buildpack-contributed profile scripts before starting the process.

use std::collections::HashMap;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::process::Command;

/// Represents a target shell execution payload.
/// Encapsulates all details required to source profiles and launch the process.
pub struct ShellProcess {
    /// If `true`, the shell runs as an indirect script session.
    pub script: bool,
    /// The executable command.
    pub command: String,
    /// Arguments to pass to the process.
    pub args: Vec<String>,
    /// The name of the calling binary.
    pub caller: String,
    /// List of profile shell scripts (`profile.d/*.sh`, `.profile`) to source before launch.
    pub profiles: Vec<String>,
    /// The target environment variable map.
    pub env: HashMap<String, String>,
    /// Custom working directory.
    pub working_directory: String,
}

/// Defines the shell runner trait to launch command sessions.
pub trait Shell {
    /// Replaces the current process image or executes the command inside a shell environment.
    ///
    /// # Arguments
    ///
    /// * `proc` - The [`ShellProcess`] containing shell wrapper parameters.
    ///
    /// # Errors
    ///
    /// Returns [`std::io::Error`] if execution or shell spawning fails.
    fn launch(&self, proc: ShellProcess) -> Result<(), std::io::Error>;
}

/// A Bash-based Unix shell executor.
/// Sources profile scripts dynamically and spawns the target process.
#[cfg(unix)]
pub struct BashShell;

#[cfg(unix)]
impl Shell for BashShell {
    fn launch(&self, proc: ShellProcess) -> Result<(), std::io::Error> {
        let mut script = String::new();
        for profile in &proc.profiles {
            script.push_str(&format!("source \"{}\"\n", profile));
        }
        script.push_str(&format!("cd \"{}\"\n", proc.working_directory));

        let bash_command = if proc.script {
            "exec bash -c \"$@\"".to_string()
        } else {
            bash_command_with_tokens(proc.args.len() + 1)
        };
        script.push_str(&bash_command);

        let mut cmd = Command::new("/bin/bash");
        cmd.arg0("bash");
        cmd.arg("-c");
        cmd.arg(script);
        cmd.arg(proc.caller);
        cmd.arg(proc.command);
        cmd.args(proc.args);

        cmd.env_clear();
        cmd.envs(&proc.env);

        let err = cmd.exec();
        Err(err)
    }
}

/// A Windows Command Prompt-based shell executor.
/// Sourced batch profile scripts and spawns the target process.
#[cfg(windows)]
pub struct CmdShell;

#[cfg(windows)]
impl Shell for CmdShell {
    fn launch(&self, proc: ShellProcess) -> Result<(), std::io::Error> {
        use std::os::windows::process::CommandExt;

        let mut parts = Vec::new();

        // 1. Profile script sourcing
        for profile in &proc.profiles {
            parts.push("call".to_string());
            parts.push(escape_msvc_arg(profile));
            parts.push("&&".to_string());
        }

        // 2. Working directory transition
        parts.push("cd".to_string());
        parts.push("/d".to_string());
        parts.push(escape_msvc_arg(&proc.working_directory));
        parts.push("&&".to_string());

        // 3. Command and its arguments
        parts.push(escape_msvc_arg(&proc.command));
        for arg in &proc.args {
            parts.push(escape_msvc_arg(arg));
        }

        let cmd_line = parts.join(" ");

        let mut cmd = Command::new("cmd");
        cmd.arg("/q");
        cmd.arg("/v:on");
        cmd.arg("/s");
        cmd.arg("/c");

        // Wrap the entire command line in outer quotes.
        // cmd.exe /s /c will strip the outer quotes and safely run the inner escaped command.
        cmd.raw_arg(&format!("\"{}\"", cmd_line));

        cmd.env_clear();
        cmd.envs(&proc.env);

        let mut child = cmd.spawn()?;
        let status = child.wait()?;
        std::process::exit(status.code().unwrap_or(0));
    }
}

/// Escapes a command line argument using standard MSVC escaping rules,
/// but also forces quoting if Windows shell metacharacters or quotes are present.
#[cfg(any(windows, test))]
fn escape_msvc_arg(arg: &str) -> String {
    if arg.is_empty() {
        return "\"\"".to_string();
    }

    let needs_quoting = arg.chars().any(|c| {
        c == ' '
            || c == '\t'
            || c == '\n'
            || c == '\x0b'
            || c == '"'
            || c == '&'
            || c == '|'
            || c == '<'
            || c == '>'
            || c == '^'
            || c == '%'
            || c == '!'
            || c == '('
            || c == ')'
    });

    if !needs_quoting {
        return arg.to_string();
    }

    let mut escaped = String::new();
    escaped.push('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        if c == '\\' {
            backslashes += 1;
        } else if c == '"' {
            for _ in 0..backslashes {
                escaped.push('\\');
                escaped.push('\\');
            }
            escaped.push('\\');
            escaped.push('"');
            backslashes = 0;
        } else {
            for _ in 0..backslashes {
                escaped.push('\\');
            }
            escaped.push(c);
            backslashes = 0;
        }
    }
    for _ in 0..backslashes {
        escaped.push('\\');
        escaped.push('\\');
    }
    escaped.push('"');
    escaped
}

/// Generates the shell argument-evaluation bash command, mimicking Go's `bashCommandWithTokens`.
///
/// In the upstream Go reference implementation, the command executable and each of its arguments
/// are referred to as "tokens". This function creates a bash script that evaluates each command token
/// (positional parameter) individually using `eval echo` to perform shell parameter expansion
/// while preserving argument boundaries.
pub fn bash_command_with_tokens(n_tokens: usize) -> String {
    let mut command_script = String::from("\"$(eval echo \\\"$0\\\")\"");
    for i in 1..n_tokens {
        command_script.push_str(&format!(" \"$(eval echo \\\"${{{}}}\\\")\"", i));
    }
    format!("exec bash -c '{}' \"${{@:1}}\"", command_script)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bash_command_with_tokens() {
        assert_eq!(
            bash_command_with_tokens(1),
            "exec bash -c '\"$(eval echo \\\"$0\\\")\"' \"${@:1}\""
        );
        assert_eq!(
            bash_command_with_tokens(3),
            "exec bash -c '\"$(eval echo \\\"$0\\\")\" \"$(eval echo \\\"${1}\\\")\" \"$(eval echo \\\"${2}\\\")\"' \"${@:1}\""
        );
    }

    #[test]
    fn test_escape_msvc_arg() {
        assert_eq!(escape_msvc_arg(""), "\"\"");
        assert_eq!(escape_msvc_arg("simple"), "simple");
        assert_eq!(escape_msvc_arg("space arg"), "\"space arg\"");
        assert_eq!(
            escape_msvc_arg("arg&with&specials"),
            "\"arg&with&specials\""
        );
        assert_eq!(escape_msvc_arg("a\\b"), "a\\b");
        assert_eq!(escape_msvc_arg("a\\ b"), "\"a\\ b\"");
        assert_eq!(escape_msvc_arg("a\\\\b"), "a\\\\b");
        assert_eq!(escape_msvc_arg("a\\\\ b"), "\"a\\\\ b\"");
        assert_eq!(escape_msvc_arg("a\"b"), "\"a\\\"b\"");
        assert_eq!(escape_msvc_arg("a\\\"b"), "\"a\\\\\\\"b\"");
        assert_eq!(escape_msvc_arg("a\\\\\"b"), "\"a\\\\\\\\\\\"b\"");
        assert_eq!(escape_msvc_arg("a\\"), "a\\");
        assert_eq!(escape_msvc_arg("a\\\\"), "a\\\\");
        assert_eq!(escape_msvc_arg("a \\"), "\"a \\\\\"");
    }
}

#[cfg(test)]
mod ported_rust_tests {
    use super::*;
    use std::collections::HashMap;

    // Ported from launcher-rust src/launch/bash.rs::tokens_n1 (and the existing
    // launcher-rs test_bash_command_with_tokens n=1 pin). Mirrors Go
    // bashCommandWithTokens(1): a single $0 token, byte-for-byte.
    #[test]
    fn bash_command_with_tokens_n1() {
        assert_eq!(
            bash_command_with_tokens(1),
            "exec bash -c '\"$(eval echo \\\"$0\\\")\"' \"${@:1}\""
        );
    }

    // Ported from launcher-rust src/launch/bash.rs::tokens_n3. Mirrors Go
    // bashCommandWithTokens(3): $0 plus ${1} ${2} eval-echo tokens, byte-for-byte.
    #[test]
    fn bash_command_with_tokens_n3() {
        assert_eq!(
            bash_command_with_tokens(3),
            "exec bash -c '\"$(eval echo \\\"$0\\\")\" \"$(eval echo \\\"${1}\\\")\" \"$(eval echo \\\"${2}\\\")\"' \"${@:1}\""
        );
    }

    // Ported (boundary variant) from launcher-rust src/launch/bash.rs token tests.
    // For n_tokens=0 the per-token loop `1..0` is empty, so the script degenerates
    // to the single $0 token form -- identical output to n=1. Go's
    // bashCommandWithTokens has the same `for i := 1; i < nTokens` boundary.
    #[test]
    fn bash_command_with_tokens_n0() {
        assert_eq!(
            bash_command_with_tokens(0),
            "exec bash -c '\"$(eval echo \\\"$0\\\")\"' \"${@:1}\""
        );
    }

    // Ported from launcher-rust src/launch/shell.rs::shell_process_struct_constructible.
    // launcher-rs ShellProcess has a divergent field shape (env: HashMap<String,String>
    // not Vec<String>; profiles: Vec<String> not Vec<PathBuf>; working_directory not
    // working_dir), so the literal is re-expressed against launcher-rs fields. Asserts
    // the struct is constructible and round-trips script + env, matching the Go-faithful
    // intent (script==true, env carries A=1).
    #[test]
    fn shell_process_struct_builds() {
        let mut env = HashMap::new();
        env.insert("A".to_string(), "1".to_string());
        let p = ShellProcess {
            script: true,
            command: "echo hi".to_string(),
            args: vec![],
            env,
            working_directory: "/app".to_string(),
            profiles: vec!["/p".to_string()],
            caller: "/cnb/lifecycle/launcher".to_string(),
        };
        assert!(p.script);
        assert_eq!(p.env.get("A"), Some(&"1".to_string()));
    }
}

