//! Layout configuration: the built-in layouts plus an optional user file.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

const BUILTIN: &str = include_str!("builtin.toml");

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default)]
    pub layouts: BTreeMap<String, Layout>,
}

/// Settings that apply to every layout unless the layout sets its own.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    /// Environment used by `up`, `shell` and `run` when none is named.
    pub env: Option<String>,
    /// ROS setup file (or install directory) sourced in every pane, e.g. `/opt/ros/jazzy`.
    pub ros_setup: Option<String>,
    /// Shell lines run in every pane before anything else.
    #[serde(default)]
    pub pre: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layout {
    #[serde(default)]
    pub description: String,
    /// Environment to load, instead of the default one.
    pub env: Option<String>,
    /// Workspace to overlay and `cd` into.
    pub workspace: Option<String>,
    pub ros_setup: Option<String>,
    #[serde(default)]
    pub pre: Vec<String>,
    /// Packages that must be built in `<workspace>/install` before starting.
    #[serde(default)]
    pub requires: Vec<String>,
    /// Shown when a required package is missing.
    pub hint: Option<String>,
    pub panes: Vec<Pane>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pane {
    /// Command to run. Empty leaves a ready shell.
    #[serde(default)]
    pub run: String,
    /// Seconds to wait before running, so earlier panes can start their nodes.
    #[serde(default)]
    pub delay: u32,
    /// Give this pane the keyboard when the session opens.
    #[serde(default)]
    pub focus: bool,
}

impl Config {
    pub fn parse(text: &str, origin: &str) -> Result<Self> {
        let config: Self =
            toml::from_str(text).with_context(|| format!("invalid config in {origin}"))?;
        for (name, layout) in &config.layouts {
            layout
                .validate()
                .with_context(|| format!("invalid layout `{name}` in {origin}"))?;
        }
        Ok(config)
    }

    pub fn builtin() -> Self {
        Self::parse(BUILTIN, "built-in layouts").expect("built-in layouts are valid")
    }

    /// Built-in layouts, overlaid with the user's file if there is one.
    pub fn load(explicit: Option<&Path>) -> Result<Self> {
        let mut config = Self::builtin();
        let path = match explicit {
            Some(path) => Some(path.to_path_buf()),
            None => default_path().filter(|path| path.exists()),
        };
        if let Some(path) = path {
            let text = fs::read_to_string(&path)
                .with_context(|| format!("cannot read {}", path.display()))?;
            config.merge(Self::parse(&text, &path.display().to_string())?);
        }
        Ok(config)
    }

    fn merge(&mut self, other: Config) {
        if other.defaults.ros_setup.is_some() {
            self.defaults.ros_setup = other.defaults.ros_setup;
        }
        if other.defaults.env.is_some() {
            self.defaults.env = other.defaults.env;
        }
        if !other.defaults.pre.is_empty() {
            self.defaults.pre = other.defaults.pre;
        }
        self.layouts.extend(other.layouts);
    }

    pub fn layout(&self, name: &str) -> Result<&Layout> {
        match self.layouts.get(name) {
            Some(layout) => Ok(layout),
            None => {
                let names: Vec<_> = self.layouts.keys().map(String::as_str).collect();
                bail!("unknown layout `{name}` (available: {})", names.join(", "))
            }
        }
    }
}

impl Layout {
    fn validate(&self) -> Result<()> {
        if self.panes.is_empty() {
            bail!("a layout needs at least one pane");
        }
        if self.panes.iter().filter(|pane| pane.focus).count() > 1 {
            bail!("only one pane can have `focus = true`");
        }
        if !self.requires.is_empty() && self.workspace.is_none() {
            bail!("`requires` needs a `workspace` to look in");
        }
        Ok(())
    }

    /// Index of the pane that gets the keyboard.
    pub fn focus(&self) -> usize {
        self.panes.iter().position(|pane| pane.focus).unwrap_or(0)
    }
}

/// `$XDG_CONFIG_HOME/ruckus`, else `~/.config/ruckus`.
pub fn config_dir() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
    };
    Some(base.join("ruckus"))
}

pub fn default_path() -> Option<PathBuf> {
    Some(config_dir()?.join("config.toml"))
}

/// Expand a leading `~` to `$HOME`.
pub fn expand_tilde(path: &str) -> PathBuf {
    let home = std::env::var_os("HOME");
    match (path.strip_prefix('~'), home) {
        (Some(""), Some(home)) => PathBuf::from(home),
        (Some(rest), Some(home)) if rest.starts_with('/') => PathBuf::from(home).join(&rest[1..]),
        _ => PathBuf::from(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_layouts_parse() {
        let config = Config::builtin();
        for name in ["turtle", "pubsub", "robot"] {
            assert!(config.layouts.contains_key(name), "missing {name}");
        }
        assert_eq!(config.layout("turtle").unwrap().focus(), 1);
    }

    #[test]
    fn user_layouts_override_builtin_by_name() {
        let mut config = Config::builtin();
        let user = Config::parse(
            r#"
            [defaults]
            ros_setup = "/opt/ros/jazzy/setup.bash"

            [layouts.turtle]
            panes = [{ run = "echo mine" }]
            "#,
            "test",
        )
        .unwrap();
        config.merge(user);
        assert_eq!(config.layout("turtle").unwrap().panes[0].run, "echo mine");
        assert!(config.layouts.contains_key("robot"));
        assert_eq!(
            config.defaults.ros_setup.as_deref(),
            Some("/opt/ros/jazzy/setup.bash")
        );
    }

    #[test]
    fn rejects_bad_layouts() {
        let cases = [
            ("[layouts.a]\npanes = []", "at least one pane"),
            (
                "[layouts.a]\npanes = [{ focus = true }, { focus = true }]",
                "only one pane",
            ),
            (
                "[layouts.a]\nrequires = [\"x\"]\npanes = [{}]",
                "needs a `workspace`",
            ),
            ("[layouts.a]\npanes = [{ cmd = \"x\" }]", "unknown field"),
        ];
        for (text, expected) in cases {
            let err = format!("{:#}", Config::parse(text, "test").unwrap_err());
            assert!(
                err.contains(expected),
                "{err:?} should mention {expected:?}"
            );
        }
    }

    #[test]
    fn unknown_layout_lists_available() {
        let err = Config::builtin().layout("nope").unwrap_err().to_string();
        assert!(err.contains("pubsub, robot, turtle"), "{err}");
    }

    #[test]
    fn expands_tilde() {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        assert_eq!(expand_tilde("~"), home);
        assert_eq!(expand_tilde("~/ws"), home.join("ws"));
        assert_eq!(expand_tilde("/abs/~x"), PathBuf::from("/abs/~x"));
        assert_eq!(expand_tilde("~other/ws"), PathBuf::from("~other/ws"));
    }
}
