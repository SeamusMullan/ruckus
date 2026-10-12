//! Named environments: a ROS install plus workspaces and settings.
//!
//! Each one is a TOML file in `~/.config/ruckus/envs/<name>.toml`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::config::{config_dir, expand_tilde};

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Env {
    /// ROS setup file, or an install directory holding `setup.<shell>`.
    pub ros: Option<String>,
    /// Workspaces overlaid on ROS, in order. The last is the working directory.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspaces: Vec<String>,
    /// Shell lines run before ROS is loaded.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pre: Vec<String>,
    /// Variables exported after ROS is loaded, e.g. `ROS_DOMAIN_ID`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub vars: BTreeMap<String, String>,
}

impl Env {
    pub fn ros_path(&self) -> Option<PathBuf> {
        self.ros.as_deref().map(expand_tilde)
    }

    pub fn workspace_paths(&self) -> Vec<PathBuf> {
        self.workspaces.iter().map(|ws| expand_tilde(ws)).collect()
    }

    fn validate(&self) -> Result<()> {
        for name in self.vars.keys() {
            let valid = name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !name.starts_with(|c: char| c.is_ascii_digit())
                && !name.is_empty();
            if !valid {
                bail!("`{name}` is not a valid variable name");
            }
        }
        Ok(())
    }

    pub fn load(name: &str) -> Result<Self> {
        let path = path(name)?;
        if !path.exists() {
            let known = list()?
                .into_iter()
                .map(|(name, _)| name)
                .collect::<Vec<_>>();
            if known.is_empty() {
                bail!("no environment named `{name}`; create one with `ruckus env create`");
            }
            bail!(
                "no environment named `{name}` (available: {})",
                known.join(", ")
            );
        }
        Self::read(&path)
    }

    fn read(path: &Path) -> Result<Self> {
        let text =
            fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
        let env: Self = toml::from_str(&text)
            .with_context(|| format!("invalid environment {}", path.display()))?;
        env.validate()
            .with_context(|| format!("invalid environment {}", path.display()))?;
        Ok(env)
    }

    /// Write the environment. Refuses to replace an existing one unless `force`.
    pub fn save(&self, name: &str, force: bool) -> Result<PathBuf> {
        self.validate()?;
        let path = path(name)?;
        if path.exists() && !force {
            bail!("environment `{name}` already exists (use --force to replace it)");
        }
        let dir = path.parent().expect("env path has a parent");
        fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        let text = format!(
            "# ruckus environment `{name}`. Edit freely; see `ruckus env show {name}`.\n{}",
            toml::to_string(self)?
        );
        fs::write(&path, text).with_context(|| format!("cannot write {}", path.display()))?;
        Ok(path)
    }
}

pub fn dir() -> Result<PathBuf> {
    Ok(config_dir().context("HOME is not set")?.join("envs"))
}

pub fn path(name: &str) -> Result<PathBuf> {
    let valid = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !valid {
        bail!("environment names use letters, digits, `-` and `_`; got `{name}`");
    }
    Ok(dir()?.join(format!("{name}.toml")))
}

/// All environments, sorted by name.
pub fn list() -> Result<Vec<(String, Env)>> {
    let dir = dir()?;
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut envs = vec![];
    for entry in fs::read_dir(&dir).with_context(|| format!("cannot read {}", dir.display()))? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "toml")
            && let Some(name) = path.file_stem().and_then(|stem| stem.to_str())
        {
            envs.push((name.to_string(), Env::read(&path)?));
        }
    }
    envs.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(envs)
}

pub fn remove(name: &str) -> Result<()> {
    let path = path(name)?;
    if !path.exists() {
        bail!("no environment named `{name}`");
    }
    fs::remove_file(&path).with_context(|| format!("cannot remove {}", path.display()))
}

/// Make a user-typed path absolute, keeping it readable as `~/...` when under `$HOME`.
pub fn store_path(path: &Path) -> Result<String> {
    let path = std::path::absolute(expand_tilde(&path.to_string_lossy()))?;
    if let Some(home) = std::env::var_os("HOME")
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return Ok(if rest.as_os_str().is_empty() {
            "~".into()
        } else {
            format!("~/{}", rest.display())
        });
    }
    Ok(path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_toml() {
        let env = Env {
            ros: Some("~/ros2_lyrical/install".into()),
            workspaces: vec!["~/lab_ws".into()],
            pre: vec![],
            vars: BTreeMap::from([("ROS_DOMAIN_ID".into(), "7".into())]),
        };
        let text = toml::to_string(&env).unwrap();
        assert!(!text.contains("pre"), "empty fields are left out:\n{text}");
        assert_eq!(toml::from_str::<Env>(&text).unwrap(), env);
    }

    #[test]
    fn rejects_bad_names() {
        for name in ["", "a b", "../x", "a/b", "x.toml"] {
            assert!(path(name).is_err(), "{name:?} should be rejected");
        }
        for var in ["", "1A", "A-B", "A B"] {
            let env = Env {
                vars: BTreeMap::from([(var.into(), "x".into())]),
                ..Env::default()
            };
            assert!(env.validate().is_err(), "{var:?} should be rejected");
        }
    }

    #[test]
    fn stores_paths_under_home_with_tilde() {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        assert_eq!(store_path(&home.join("ws")).unwrap(), "~/ws");
        assert_eq!(store_path(Path::new("~/ws")).unwrap(), "~/ws");
        assert_eq!(store_path(Path::new("/opt/ros")).unwrap(), "/opt/ros");
    }
}
