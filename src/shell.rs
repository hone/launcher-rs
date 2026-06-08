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
        let mut command_tokens = Vec::new();
        for profile in &proc.profiles {
            command_tokens.push("call".to_string());
            command_tokens.push(profile.clone());
            command_tokens.push("&&".to_string());
        }
        command_tokens.push("cd".to_string());
        command_tokens.push("/d".to_string());
        command_tokens.push(proc.working_directory.clone());
        command_tokens.push("&&".to_string());
        command_tokens.push(proc.command.clone());
        command_tokens.extend(proc.args.clone());

        let mut cmd = Command::new("cmd");
        cmd.arg("/q");
        cmd.arg("/v:on");
        cmd.arg("/s");
        cmd.arg("/c");
        cmd.args(&command_tokens);

        cmd.env_clear();
        cmd.envs(&proc.env);

        let mut child = cmd.spawn()?;
        let status = child.wait()?;
        std::process::exit(status.code().unwrap_or(0));
    }
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
}
