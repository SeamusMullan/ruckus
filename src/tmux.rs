//! Turn a layout into tmux commands and run them.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::config::{Defaults, Layout, expand_tilde};

/// Values from the command line that override the layout.
#[derive(Debug, Default)]
pub struct Overrides {
    pub workspace: Option<PathBuf>,
    pub ros_setup: Option<PathBuf>,
}

/// Everything needed to start one session, resolved and checked.
#[derive(Debug)]
pub struct Plan {
    pub session: String,
    pub window: String,
    pub start_dir: Option<PathBuf>,
    /// Shell line that loads ROS and the workspace in each pane.
    pub setup: String,
    pub commands: Vec<String>,
    pub focus: usize,
}

impl Plan {
    pub fn new(
        name: &str,
        session: &str,
        layout: &Layout,
        defaults: &Defaults,
        overrides: &Overrides,
    ) -> Result<Self> {
        let workspace = overrides
            .workspace
            .clone()
            .or_else(|| layout.workspace.as_deref().map(expand_tilde));
        let ros_setup = overrides.ros_setup.clone().or_else(|| {
            layout
                .ros_setup
                .as_deref()
                .or(defaults.ros_setup.as_deref())
                .map(expand_tilde)
        });

        if let Some(ros_setup) = &ros_setup
            && !ros_setup.is_file()
        {
            bail!("ROS setup file not found: {}", ros_setup.display());
        }
        if let Some(workspace) = &workspace {
            check_requires(workspace, &layout.requires, layout.hint.as_deref())?;
        }

        let pre = if layout.pre.is_empty() {
            &defaults.pre
        } else {
            &layout.pre
        };
        let setup = setup_line(pre, ros_setup.as_deref(), workspace.as_deref());
        let commands = layout
            .panes
            .iter()
            .map(|pane| match (pane.delay, pane.run.is_empty()) {
                (_, true) => String::new(),
                (0, false) => pane.run.clone(),
                (delay, false) => format!("sleep {delay}; {}", pane.run),
            })
            .collect();

        Ok(Self {
            session: session.to_string(),
            window: name.to_string(),
            start_dir: workspace,
            setup,
            commands,
            focus: layout.focus(),
        })
    }

    /// Create the session, split it into tiled panes and start each command.
    pub fn execute(&self, tmux: &mut impl Tmux) -> Result<()> {
        let dir_args: Vec<String> = match &self.start_dir {
            Some(dir) => vec!["-c".into(), dir.display().to_string()],
            None => vec![],
        };

        let mut new_session = args(&[
            "new-session",
            "-d",
            "-P",
            "-F",
            "#{pane_id}",
            "-x",
            "200",
            "-y",
            "50",
            "-s",
        ]);
        new_session.extend([self.session.clone(), "-n".into(), self.window.clone()]);
        new_session.extend(dir_args.iter().cloned());
        let first = tmux.run(&new_session)?;

        let mut panes = vec![first.clone()];
        for _ in 1..self.commands.len() {
            // Split the newest pane: tmux inserts the new pane right after its target,
            // so this keeps pane order the same as the layout's order.
            let mut split = args(&["split-window", "-P", "-F", "#{pane_id}", "-t"]);
            split.push(panes[panes.len() - 1].clone());
            split.extend(dir_args.iter().cloned());
            panes.push(tmux.run(&split)?);
            // Re-tile after each split so many panes still fit.
            tmux.run(&args(&["select-layout", "-t", &first, "tiled"]))?;
        }

        for (pane, command) in panes.iter().zip(&self.commands) {
            if !self.setup.is_empty() {
                tmux.run(&args(&["send-keys", "-t", pane, &self.setup, "Enter"]))?;
            }
            if !command.is_empty() {
                tmux.run(&args(&["send-keys", "-t", pane, command, "Enter"]))?;
            }
        }

        tmux.run(&args(&["set-option", "-t", &self.session, "mouse", "on"]))?;
        tmux.run(&args(&["select-pane", "-t", &panes[self.focus]]))?;
        Ok(())
    }
}

fn check_requires(workspace: &Path, requires: &[String], hint: Option<&str>) -> Result<()> {
    let missing: Vec<&str> = requires
        .iter()
        .filter(|package| !workspace.join("install").join(package).is_dir())
        .map(String::as_str)
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    let mut message = format!(
        "not built in {}: {}",
        workspace.display(),
        missing.join(", ")
    );
    if let Some(hint) = hint {
        message.push_str("\nhint: ");
        message.push_str(hint);
    }
    bail!(message)
}

/// `pre; source <ros>; source <ws>/install/setup.<ext>; cd <ws>; clear`
fn setup_line(pre: &[String], ros_setup: Option<&Path>, workspace: Option<&Path>) -> String {
    let mut parts: Vec<String> = pre.to_vec();
    if let Some(ros_setup) = ros_setup {
        parts.push(format!(
            "source {}",
            quote(&ros_setup.display().to_string())
        ));
    }
    if let Some(workspace) = workspace {
        // Match the overlay to the shell the ROS setup file is written for.
        let ext = ros_setup
            .and_then(Path::extension)
            .and_then(|ext| ext.to_str())
            .unwrap_or_else(|| shell_ext());
        let overlay = workspace.join("install").join(format!("setup.{ext}"));
        if overlay.is_file() {
            parts.push(format!("source {}", quote(&overlay.display().to_string())));
        }
        parts.push(format!("cd {}", quote(&workspace.display().to_string())));
    }
    if !parts.is_empty() {
        parts.push("clear".into());
    }
    parts.join("; ")
}

fn shell_ext() -> &'static str {
    let shell = std::env::var("SHELL").unwrap_or_default();
    if shell.ends_with("zsh") {
        "zsh"
    } else if shell.ends_with("bash") {
        "bash"
    } else {
        "sh"
    }
}

/// Quote for a POSIX shell, leaving plain words alone.
pub fn quote(word: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "-_./=:,@%+".contains(c);
    if !word.is_empty() && word.chars().all(plain) {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

fn args(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| part.to_string()).collect()
}

/// Something that runs tmux commands. Swapped for a recorder in dry runs and tests.
pub trait Tmux {
    /// Run `tmux <args>` and return its trimmed stdout.
    fn run(&mut self, args: &[String]) -> Result<String>;

    fn has_session(&mut self, session: &str) -> bool {
        self.run(&args(&["has-session", "-t", &format!("={session}")]))
            .is_ok()
    }
}

/// The real tmux. `RUCKUS_TMUX_SOCKET` selects a separate server (`tmux -L`), used by tests.
pub struct RealTmux {
    socket: Option<String>,
}

impl RealTmux {
    pub fn from_env() -> Self {
        Self {
            socket: std::env::var("RUCKUS_TMUX_SOCKET")
                .ok()
                .filter(|s| !s.is_empty()),
        }
    }

    pub fn command(&self) -> Command {
        let mut command = Command::new("tmux");
        if let Some(socket) = &self.socket {
            command.args(["-L", socket]);
        }
        command
    }
}

impl Tmux for RealTmux {
    fn run(&mut self, args: &[String]) -> Result<String> {
        let output = self
            .command()
            .args(args)
            .output()
            .context("cannot run tmux; is it installed?")?;
        if !output.status.success() {
            bail!(
                "tmux {} failed: {}",
                args.first().map(String::as_str).unwrap_or_default(),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }
}

/// Records commands instead of running them, handing out fake pane ids.
#[derive(Default)]
pub struct Recorder {
    pub calls: Vec<Vec<String>>,
    panes: usize,
}

impl Tmux for Recorder {
    fn run(&mut self, args: &[String]) -> Result<String> {
        self.calls.push(args.to_vec());
        if args.iter().any(|arg| arg == "-P") {
            self.panes += 1;
            return Ok(format!("%{}", self.panes - 1));
        }
        Ok(String::new())
    }

    fn has_session(&mut self, _session: &str) -> bool {
        false
    }
}

impl Recorder {
    /// The recorded calls as copy-pasteable shell lines.
    pub fn script(&self) -> String {
        self.calls
            .iter()
            .map(|call| {
                let words: Vec<String> = call.iter().map(|word| quote(word)).collect();
                format!("tmux {}\n", words.join(" "))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn layout(text: &str) -> Layout {
        Config::parse(text, "test")
            .unwrap()
            .layouts
            .remove("t")
            .unwrap()
    }

    #[test]
    fn quotes_only_when_needed() {
        assert_eq!(quote("ros2"), "ros2");
        assert_eq!(quote("/opt/ros/setup.bash"), "/opt/ros/setup.bash");
        assert_eq!(quote("a b"), "'a b'");
        assert_eq!(quote("it's"), r"'it'\''s'");
        assert_eq!(quote(""), "''");
    }

    #[test]
    fn builds_tiled_session_with_delays_and_focus() {
        let layout = layout(
            r#"[layouts.t]
            panes = [{ run = "a" }, { run = "b", focus = true }, { run = "c", delay = 2 }, {}]"#,
        );
        let plan = Plan::new(
            "t",
            "ruckus-t",
            &layout,
            &Defaults::default(),
            &Overrides::default(),
        )
        .unwrap();
        assert_eq!(plan.commands, ["a", "b", "sleep 2; c", ""]);
        assert_eq!(plan.setup, "");

        let mut tmux = Recorder::default();
        plan.execute(&mut tmux).unwrap();
        let script = tmux.script();

        assert!(
            script.starts_with("tmux new-session -d -P -F '#{pane_id}'"),
            "{script}"
        );
        assert_eq!(script.matches("split-window").count(), 3);
        assert!(
            script.contains("tmux send-keys -t %2 'sleep 2; c' Enter"),
            "{script}"
        );
        // An empty pane gets no command and no setup line.
        assert!(!script.contains("send-keys -t %3"), "{script}");
        assert!(script.ends_with("tmux select-pane -t %1\n"), "{script}");
    }

    #[test]
    fn setup_sources_ros_and_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let ros = dir.path().join("setup.bash");
        std::fs::write(&ros, "").unwrap();
        let ws = dir.path().join("ws");
        std::fs::create_dir_all(ws.join("install/pkg")).unwrap();
        std::fs::write(ws.join("install/setup.bash"), "").unwrap();

        let layout = layout(
            r#"[layouts.t]
            requires = ["pkg"]
            workspace = "unused"
            panes = [{ run = "x" }]"#,
        );
        let defaults = Defaults {
            ros_setup: None,
            pre: vec!["export A=1".into()],
        };
        let overrides = Overrides {
            workspace: Some(ws.clone()),
            ros_setup: Some(ros.clone()),
        };
        let plan = Plan::new("t", "s", &layout, &defaults, &overrides).unwrap();
        assert_eq!(
            plan.setup,
            format!(
                "export A=1; source {}; source {}; cd {}; clear",
                ros.display(),
                ws.join("install/setup.bash").display(),
                ws.display()
            )
        );
        assert_eq!(plan.start_dir.as_deref(), Some(ws.as_path()));
    }

    #[test]
    fn missing_package_fails_with_hint() {
        let dir = tempfile::tempdir().unwrap();
        let layout = layout(
            r#"[layouts.t]
            requires = ["a", "b"]
            workspace = "unused"
            hint = "build it"
            panes = [{}]"#,
        );
        std::fs::create_dir_all(dir.path().join("install/a")).unwrap();
        let overrides = Overrides {
            workspace: Some(dir.path().to_path_buf()),
            ros_setup: None,
        };
        let err = Plan::new("t", "s", &layout, &Defaults::default(), &overrides)
            .unwrap_err()
            .to_string();
        assert!(err.contains(": b\nhint: build it"), "{err}");
    }

    #[test]
    fn missing_ros_setup_fails() {
        let layout = layout("[layouts.t]\npanes = [{}]");
        let overrides = Overrides {
            workspace: None,
            ros_setup: Some("/nonexistent/setup.zsh".into()),
        };
        let err = Plan::new("t", "s", &layout, &Defaults::default(), &overrides).unwrap_err();
        assert!(err.to_string().contains("ROS setup file not found"));
    }
}
