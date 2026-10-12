//! Shells, and the lines that load ROS into one.
//!
//! ROS is always loaded by sourcing its setup files *inside* the target shell,
//! never by copying variables in from outside. On macOS, System Integrity
//! Protection strips `DYLD_*` variables whenever a protected binary such as
//! `/bin/zsh` starts, so variables passed in from outside would be lost.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::tmux::quote;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shell {
    Zsh,
    Bash,
    Sh,
}

impl Shell {
    /// The user's shell, from `$SHELL`.
    pub fn from_env() -> Self {
        Self::from_path(&std::env::var("SHELL").unwrap_or_default())
    }

    pub fn from_path(path: &str) -> Self {
        let name = Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        match name {
            "zsh" => Self::Zsh,
            "bash" => Self::Bash,
            _ => Self::Sh,
        }
    }

    /// Extension of the ROS setup file written for this shell.
    pub fn ext(self) -> &'static str {
        match self {
            Self::Zsh => "zsh",
            Self::Bash => "bash",
            Self::Sh => "sh",
        }
    }

    /// The `$SHELL` binary if it is this shell, else the shell by name.
    fn program(self) -> String {
        match std::env::var("SHELL") {
            Ok(path) if Self::from_path(&path) == self => path,
            _ => self.ext().to_string(),
        }
    }
}

/// What to run in a shell to load ROS, workspaces and settings.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Activation {
    pub pre: Vec<String>,
    pub sources: Vec<PathBuf>,
    pub vars: BTreeMap<String, String>,
}

impl Activation {
    /// Resolve a ROS install and workspaces into the setup files to source.
    ///
    /// `ros` is a setup file or an install directory holding `setup.<ext>`.
    /// Workspaces that are not built yet are skipped.
    pub fn resolve(
        ros: Option<&Path>,
        workspaces: &[PathBuf],
        pre: Vec<String>,
        vars: BTreeMap<String, String>,
        shell: Shell,
    ) -> Result<Self> {
        // A setup file names its shell; otherwise use the shell we will run in.
        let ext = ros
            .filter(|path| path.is_file())
            .and_then(Path::extension)
            .and_then(|ext| ext.to_str())
            .unwrap_or(shell.ext());
        let mut sources = vec![];
        if let Some(ros) = ros {
            sources.push(setup_file(ros, ext)?);
        }
        for workspace in workspaces {
            let overlay = workspace.join("install").join(format!("setup.{ext}"));
            if overlay.is_file() && !sources.contains(&overlay) {
                sources.push(overlay);
            }
        }
        Ok(Self { pre, sources, vars })
    }

    /// Shell lines, in order: `pre`, then `source` each file, then `export` the variables.
    pub fn lines(&self) -> Vec<String> {
        let mut lines = self.pre.clone();
        for source in &self.sources {
            lines.push(format!("source {}", quote(&source.display().to_string())));
        }
        for (name, value) in &self.vars {
            lines.push(format!("export {name}={}", quote(value)));
        }
        lines
    }
}

/// `path` itself if it is a file, else `path/setup.<ext>`.
pub fn setup_file(path: &Path, ext: &str) -> Result<PathBuf> {
    if path.is_file() {
        return Ok(path.to_path_buf());
    }
    if !path.is_dir() {
        bail!("ROS setup not found: {}", path.display());
    }
    let file = path.join(format!("setup.{ext}"));
    if !file.is_file() {
        bail!(
            "no setup.{ext} in {} (is it a ROS install directory?)",
            path.display()
        );
    }
    Ok(file)
}

/// Replace this process with an interactive shell that has `activation` loaded.
///
/// The user's own rc files still run first, so their prompt and aliases stay.
pub fn exec_interactive(name: &str, activation: &Activation, shell: Shell) -> Result<()> {
    let dir = cache_dir()?.join("shell").join(name);
    fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let mut lines = activation.lines();
    lines.push(format!("export RUCKUS_ENV={}", quote(name)));
    let body = lines.join("\n");

    let mut command = Command::new(shell.program());
    match shell {
        Shell::Zsh => {
            // zsh reads its rc files from $ZDOTDIR. Point it at wrappers that
            // source the user's files, add ROS, then hand $ZDOTDIR back.
            let user_dir = std::env::var_os("ZDOTDIR")
                .or_else(|| std::env::var_os("HOME"))
                .context("HOME is not set")?;
            let wrap = |file: &str| {
                format!(
                    "RUCKUS_ZDOTDIR=\"$ZDOTDIR\"\nZDOTDIR=\"$RUCKUS_USER_ZDOTDIR\"\n\
                     [ -f \"$ZDOTDIR/{file}\" ] && source \"$ZDOTDIR/{file}\"\n\
                     RUCKUS_USER_ZDOTDIR=\"$ZDOTDIR\"\nZDOTDIR=\"$RUCKUS_ZDOTDIR\"\n"
                )
            };
            fs::write(dir.join(".zshenv"), wrap(".zshenv"))?;
            fs::write(
                dir.join(".zshrc"),
                format!(
                    "{}{body}\nZDOTDIR=\"$RUCKUS_USER_ZDOTDIR\"\nunset RUCKUS_ZDOTDIR RUCKUS_USER_ZDOTDIR\n",
                    wrap(".zshrc")
                ),
            )?;
            command
                .env("ZDOTDIR", &dir)
                .env("RUCKUS_USER_ZDOTDIR", user_dir)
                .arg("-i");
        }
        Shell::Bash => {
            let rc = dir.join("bashrc");
            fs::write(&rc, format!("[ -f ~/.bashrc ] && . ~/.bashrc\n{body}\n"))?;
            command.arg("--rcfile").arg(rc).arg("-i");
        }
        Shell::Sh => bail!(
            "ruckus shell supports zsh and bash; $SHELL is {}",
            std::env::var("SHELL").unwrap_or_default()
        ),
    }
    eprintln!("ruckus: entering {name} (type `exit` to leave)");
    Err(command.exec()).context("cannot start the shell")
}

/// Replace this process with `program args...`, run after `activation` in a shell.
pub fn exec_command(activation: &Activation, shell: Shell, program: &[String]) -> Result<()> {
    let mut script = activation.lines();
    script.push("exec \"$@\"".into());
    let error = Command::new(shell.program())
        .arg("-c")
        .arg(script.join("\n"))
        .arg("ruckus")
        .args(program)
        .exec();
    Err(error).context("cannot start the shell")
}

/// `$XDG_CACHE_HOME/ruckus`, else `~/.cache/ruckus`.
fn cache_dir() -> Result<PathBuf> {
    let base = match std::env::var_os("XDG_CACHE_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".cache"),
    };
    Ok(base.join("ruckus"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_shell_from_path() {
        assert_eq!(Shell::from_path("/bin/zsh"), Shell::Zsh);
        assert_eq!(Shell::from_path("/opt/homebrew/bin/bash"), Shell::Bash);
        assert_eq!(Shell::from_path("/usr/bin/fish"), Shell::Sh);
        assert_eq!(Shell::from_path(""), Shell::Sh);
    }

    #[test]
    fn install_dir_resolves_to_shell_setup_file() {
        let dir = tempfile::tempdir().unwrap();
        for ext in ["zsh", "bash"] {
            fs::write(dir.path().join(format!("setup.{ext}")), "").unwrap();
        }
        let ws = dir.path().join("ws");
        fs::create_dir_all(ws.join("install")).unwrap();
        fs::write(ws.join("install/setup.zsh"), "").unwrap();
        let unbuilt = dir.path().join("unbuilt");

        let activation = Activation::resolve(
            Some(dir.path()),
            &[ws.clone(), unbuilt],
            vec![],
            BTreeMap::new(),
            Shell::Zsh,
        )
        .unwrap();
        assert_eq!(
            activation.sources,
            [dir.path().join("setup.zsh"), ws.join("install/setup.zsh")]
        );

        let bash = Activation::resolve(
            Some(dir.path()),
            &[ws],
            vec![],
            BTreeMap::new(),
            Shell::Bash,
        )
        .unwrap();
        // The workspace has no setup.bash, so only ROS is sourced.
        assert_eq!(bash.sources, [dir.path().join("setup.bash")]);
    }

    #[test]
    fn setup_file_extension_wins_over_shell() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("setup.bash");
        fs::write(&file, "").unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(ws.join("install")).unwrap();
        fs::write(ws.join("install/setup.bash"), "").unwrap();

        let activation = Activation::resolve(
            Some(&file),
            std::slice::from_ref(&ws),
            vec![],
            BTreeMap::new(),
            Shell::Zsh,
        )
        .unwrap();
        assert_eq!(activation.sources, [file, ws.join("install/setup.bash")]);
    }

    #[test]
    fn missing_ros_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = setup_file(&dir.path().join("nope"), "zsh").unwrap_err();
        assert!(err.to_string().contains("ROS setup not found"));
        let err = setup_file(dir.path(), "zsh").unwrap_err();
        assert!(err.to_string().contains("no setup.zsh in"));
    }

    #[test]
    fn lines_are_pre_then_sources_then_vars() {
        let activation = Activation {
            pre: vec!["export PATH=\"$HOME/bin:$PATH\"".into()],
            sources: vec!["/ros/my setup.zsh".into()],
            vars: BTreeMap::from([("ROS_DOMAIN_ID".into(), "7".into())]),
        };
        assert_eq!(
            activation.lines(),
            [
                "export PATH=\"$HOME/bin:$PATH\"",
                "source '/ros/my setup.zsh'",
                "export ROS_DOMAIN_ID=7",
            ]
        );
    }
}
