//! Remove a previously loaded ROS from a shell before loading another one.
//!
//! Users often source a ROS in their rc file. Loading a second distro on top
//! leaves the first one's paths behind, and ROS warns that the environment
//! mixes distributions. A shell evals the output of `ruckus internal
//! clean-env` to drop every path under the old installs and the `ROS_*`
//! variables they set. Settings the user chose, like `ROS_DOMAIN_ID`, stay.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use crate::tmux::quote;

/// Variables that list paths, cleaned entry by entry.
const PATH_VARS: &[&str] = &[
    "PATH",
    "PYTHONPATH",
    "AMENT_PREFIX_PATH",
    "COLCON_PREFIX_PATH",
    "CMAKE_PREFIX_PATH",
    "PKG_CONFIG_PATH",
    "LD_LIBRARY_PATH",
    "DYLD_LIBRARY_PATH",
    "DYLD_FALLBACK_LIBRARY_PATH",
];

/// Variables that describe the loaded ROS itself.
const ROS_VARS: &[&str] = &[
    "ROS_DISTRO",
    "ROS_VERSION",
    "ROS_PYTHON_VERSION",
    "ROS_ETC_DIR",
    "ROS_OS_OVERRIDE",
    "ROS_PACKAGE_PATH",
    "AMENT_CURRENT_PREFIX",
    "COLCON_CURRENT_PREFIX",
];

/// Shell lines that remove a loaded ROS from `env`; empty if none is loaded.
pub fn script(env: &HashMap<String, String>) -> String {
    let get = |name: &str| env.get(name).map(String::as_str).unwrap_or_default();
    let roots = roots(env);
    if roots.is_empty() && get("ROS_DISTRO").is_empty() {
        return String::new();
    }

    let mut lines = vec![];
    for name in PATH_VARS {
        let Some(value) = env.get(*name) else {
            continue;
        };
        let kept: Vec<&str> = value
            .split(':')
            .filter(|entry| {
                !entry.is_empty() && !roots.iter().any(|root| Path::new(entry).starts_with(root))
            })
            .collect();
        if kept.len() == value.split(':').filter(|e| !e.is_empty()).count() {
            continue;
        }
        if kept.is_empty() {
            lines.push(format!("unset {name}"));
        } else {
            lines.push(format!("export {name}={}", quote(&kept.join(":"))));
        }
    }
    // A conda prefix that held the old ROS goes too, so its activation is not half kept.
    let conda = get("CONDA_PREFIX");
    let mut unset: Vec<&str> = ROS_VARS
        .iter()
        .copied()
        .filter(|name| env.contains_key(*name))
        .collect();
    // Set by `ruckus install`ed distros; without it colcon reads ~/.colcon again.
    if get("COLCON_HOME").contains("/ruckus/distros/") {
        unset.push("COLCON_HOME");
    }
    if !conda.is_empty() && roots.iter().any(|root| Path::new(conda).starts_with(root)) {
        unset.extend(["CONDA_PREFIX", "CONDA_SHLVL", "CONDA_DEFAULT_ENV"]);
    }
    if !unset.is_empty() {
        lines.push(format!("unset {}", unset.join(" ")));
    }
    lines.join("\n")
}

/// Directories that hold the loaded ROS and its workspaces.
///
/// A colcon prefix like `~/ros2_lyrical/install/rclpy` widens to the
/// workspace `~/ros2_lyrical`, so its `build/` entries in PYTHONPATH go too.
fn roots(env: &HashMap<String, String>) -> Vec<PathBuf> {
    let home = env.get("HOME").map(PathBuf::from);
    let mut roots: Vec<PathBuf> = vec![];
    for name in ["AMENT_PREFIX_PATH", "COLCON_PREFIX_PATH"] {
        for entry in env
            .get(name)
            .map(String::as_str)
            .unwrap_or_default()
            .split(':')
        {
            if entry.is_empty() {
                continue;
            }
            let path = Path::new(entry);
            let root = workspace_of(path).unwrap_or_else(|| path.to_path_buf());
            // Never treat `/` or $HOME as part of ROS, whatever the variables say.
            let too_wide = root.components().count() < 3 || Some(&root) == home.as_ref();
            if !too_wide && !roots.contains(&root) {
                roots.push(root);
            }
        }
    }
    roots
}

/// For a path inside a colcon `install` directory, the workspace above it.
fn workspace_of(path: &Path) -> Option<PathBuf> {
    let mut root = PathBuf::new();
    for component in path.components() {
        if component == Component::Normal("install".as_ref()) {
            return Some(root);
        }
        root.push(component);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn nothing_loaded_means_nothing_to_do() {
        assert_eq!(
            script(&env(&[("PATH", "/usr/bin:/bin"), ("HOME", "/Users/me")])),
            ""
        );
    }

    #[test]
    fn removes_a_colcon_source_build() {
        let script = script(&env(&[
            ("HOME", "/Users/me"),
            ("ROS_DISTRO", "lyrical"),
            ("ROS_VERSION", "2"),
            ("ROS_DOMAIN_ID", "7"),
            (
                "PATH",
                "/Users/me/ros2_lyrical/install/ros2cli/bin:/Users/me/bin:/usr/bin",
            ),
            (
                "PYTHONPATH",
                "/Users/me/ros2_lyrical/build/launch:/Users/me/ros2_lyrical/install/rclpy/lib",
            ),
            (
                "AMENT_PREFIX_PATH",
                "/Users/me/ros2_lyrical/install/rclpy:/Users/me/ros2_lyrical/install/ros2cli",
            ),
            ("COLCON_PREFIX_PATH", "/Users/me/ros2_lyrical/install"),
        ]));
        assert_eq!(
            script,
            "export PATH=/Users/me/bin:/usr/bin\n\
             unset PYTHONPATH\n\
             unset AMENT_PREFIX_PATH\n\
             unset COLCON_PREFIX_PATH\n\
             unset ROS_DISTRO ROS_VERSION"
        );
    }

    #[test]
    fn removes_a_conda_ros() {
        let script = script(&env(&[
            ("HOME", "/Users/me"),
            ("ROS_DISTRO", "jazzy"),
            (
                "CONDA_PREFIX",
                "/Users/me/.local/share/ruckus/distros/jazzy/prefix",
            ),
            ("CONDA_SHLVL", "1"),
            (
                "COLCON_HOME",
                "/Users/me/.local/share/ruckus/distros/jazzy/colcon",
            ),
            (
                "AMENT_PREFIX_PATH",
                "/Users/me/.local/share/ruckus/distros/jazzy/prefix",
            ),
            (
                "PATH",
                "/Users/me/.local/share/ruckus/distros/jazzy/prefix/bin:/usr/bin",
            ),
        ]));
        assert!(script.contains("export PATH=/usr/bin\n"), "{script}");
        assert!(
            script.ends_with(
                "unset ROS_DISTRO COLCON_HOME CONDA_PREFIX CONDA_SHLVL CONDA_DEFAULT_ENV"
            ),
            "{script}"
        );
    }

    #[test]
    fn never_strips_home_or_root() {
        let script = script(&env(&[
            ("HOME", "/Users/me"),
            ("ROS_DISTRO", "odd"),
            ("AMENT_PREFIX_PATH", "/Users/me:/:/opt"),
            ("PATH", "/Users/me/bin:/usr/bin:/opt/bin"),
        ]));
        assert_eq!(script, "unset ROS_DISTRO");
    }

    #[test]
    fn quotes_paths_with_spaces() {
        let script = script(&env(&[
            ("ROS_DISTRO", "jazzy"),
            ("AMENT_PREFIX_PATH", "/opt/ros/jazzy"),
            ("PATH", "/opt/ros/jazzy/bin:/Users/me/My Tools/bin"),
        ]));
        assert!(
            script.starts_with("export PATH='/Users/me/My Tools/bin'\n"),
            "{script}"
        );
    }
}
