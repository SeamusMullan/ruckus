//! Turn a layout into tmux commands and run them.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::config::{Defaults, Layout, expand_tilde};
use crate::env::Env;
use crate::shell::{Activation, Shell};

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
    /// Resolve where ROS, the workspaces and the working directory come from.
    ///
    /// Precedence, highest first: command line, layout, environment, `[defaults]`.
    /// `pre` lines add up in the opposite order: defaults, then env, then layout.
    pub fn new(
        name: &str,
        session: &str,
        layout: &Layout,
        defaults: &Defaults,
        env: Option<&Env>,
        overrides: &Overrides,
        shell: Shell,
    ) -> Result<Self> {
        let ros = overrides
            .ros_setup
            .clone()
            .or_else(|| layout.ros_setup.as_deref().map(expand_tilde))
            .or_else(|| env.and_then(Env::ros_path))
            .or_else(|| defaults.ros_setup.as_deref().map(expand_tilde));

        let mut workspaces = env.map(Env::workspace_paths).unwrap_or_default();
        let workdir = overrides
            .workspace
            .clone()
            .or_else(|| layout.workspace.as_deref().map(expand_tilde))
            .or_else(|| workspaces.last().cloned());
        if let Some(workdir) = &workdir {
            check_requires(workdir, &layout.requires, layout.hint.as_deref())?;
            if !workspaces.contains(workdir) {
                workspaces.push(workdir.clone());
            }
        }

        let mut pre = defaults.pre.clone();
        if let Some(env) = env {
            pre.extend(env.pre.iter().cloned());
        }
        pre.extend(layout.pre.iter().cloned());
        let vars = env.map(|env| env.vars.clone()).unwrap_or_default();

        let activation = Activation::resolve(ros.as_deref(), &workspaces, pre, vars, shell)?;
        let mut setup = activation.lines();
        if let Some(workdir) = &workdir {
            setup.push(format!("cd {}", quote(&workdir.display().to_string())));
        }
        if !setup.is_empty() {
            setup.push("clear".into());
        }

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
            start_dir: workdir,
            setup: setup.join("; "),
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

    /// The first setup line whenever a ROS is loaded: unload the shell's old one.
    fn clean_line() -> String {
        let exe = std::env::current_exe().unwrap();
        format!(
            "eval \"$({} internal clean-env)\"",
            quote(&exe.display().to_string())
        )
    }
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
            None,
            &Overrides::default(),
            Shell::Bash,
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
            pre: vec!["export A=1".into()],
            ..Defaults::default()
        };
        let overrides = Overrides {
            workspace: Some(ws.clone()),
            ros_setup: Some(ros.clone()),
        };
        let plan = Plan::new("t", "s", &layout, &defaults, None, &overrides, Shell::Zsh).unwrap();
        assert_eq!(
            plan.setup,
            format!(
                "{}; export A=1; source {}; source {}; cd {}; clear",
                clean_line(),
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
        let err = Plan::new(
            "t",
            "s",
            &layout,
            &Defaults::default(),
            None,
            &overrides,
            Shell::Zsh,
        )
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
        let err = Plan::new(
            "t",
            "s",
            &layout,
            &Defaults::default(),
            None,
            &overrides,
            Shell::Zsh,
        )
        .unwrap_err();
        assert!(err.to_string().contains("ROS setup not found"));
    }

    #[test]
    fn env_supplies_ros_workspaces_and_vars() {
        let dir = tempfile::tempdir().unwrap();
        let ros = dir.path().join("ros");
        let base = dir.path().join("base_ws");
        let main = dir.path().join("main_ws");
        for prefix in [&base, &main] {
            std::fs::create_dir_all(prefix.join("install")).unwrap();
            std::fs::write(prefix.join("install/setup.zsh"), "").unwrap();
        }
        std::fs::create_dir_all(&ros).unwrap();
        std::fs::write(ros.join("setup.zsh"), "").unwrap();

        let env = Env {
            ros: Some(ros.display().to_string()),
            workspaces: vec![base.display().to_string(), main.display().to_string()],
            pre: vec!["echo env".into()],
            vars: [("ROS_DOMAIN_ID".to_string(), "7".to_string())].into(),
        };
        let defaults = Defaults {
            pre: vec!["echo defaults".into()],
            // The env's ROS wins over the default one.
            ros_setup: Some("/nonexistent".into()),
            ..Defaults::default()
        };
        let layout = layout("[layouts.t]\npre = [\"echo layout\"]\npanes = [{}]");
        let plan = Plan::new(
            "t",
            "s",
            &layout,
            &defaults,
            Some(&env),
            &Overrides::default(),
            Shell::Zsh,
        )
        .unwrap();
        assert_eq!(
            plan.setup,
            format!(
                "{}; echo defaults; echo env; echo layout; source {}; source {}; source {}; \
                 export ROS_DOMAIN_ID=7; cd {}; clear",
                clean_line(),
                ros.join("setup.zsh").display(),
                base.join("install/setup.zsh").display(),
                main.join("install/setup.zsh").display(),
                main.display()
            )
        );
        // With no workspace of its own, the layout runs in the env's last workspace.
        assert_eq!(plan.start_dir.as_deref(), Some(main.as_path()));
    }
}
