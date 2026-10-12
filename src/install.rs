//! `ruckus install`: ready-built ROS 2 from RoboStack, installed natively.
//!
//! Each distro lives in `~/.local/share/ruckus/distros/<distro>/`:
//!
//! - `prefix/`: a conda prefix, resolved and installed with rattler
//!   (the library behind pixi), so neither conda nor pixi is needed.
//! - `setup.zsh`, `setup.bash`, `setup.sh`: activation scripts, which make
//!   the directory a ROS install that environments can point at.
//! - `colcon/`: this distro's `COLCON_HOME`, so colcon settings made for
//!   another ROS in `~/.colcon` (such as its Python) do not leak in.
//! - `ruckus.toml`: what was installed, written last. A directory without it
//!   is an interrupted install; running `ruckus install` again finishes it.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use indicatif::{ProgressBar, ProgressStyle};
use rattler::install::{IndicatifReporter, Installer};
use rattler_cache::package_cache::PackageCache;
use rattler_conda_types::{
    Channel, ChannelConfig, GenericVirtualPackage, MatchSpec, ParseStrictness, RepoDataRecord,
    Subdir,
};
use rattler_repodata_gateway::{Gateway, GatewayError};
use rattler_shell::activation::{ActivationVariables, Activator, PathModificationBehavior};
use rattler_shell::shell::{Bash, Shell, Zsh};
use rattler_solve::{SolveError, SolverImpl, SolverTask, resolvo};
use rattler_virtual_packages::{VirtualPackage, VirtualPackageOverrides};
use serde::{Deserialize, Serialize};

use crate::tmux::quote;

/// Which ROS metapackage to install.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Variant {
    /// Everything in base, plus RViz, turtlesim, demos and tutorials
    Desktop,
    /// Core libraries and command line tools, no GUI tools
    Base,
}

impl Variant {
    fn package(self, distro: &str) -> String {
        match self {
            Self::Desktop => format!("ros-{distro}-desktop"),
            Self::Base => format!("ros-{distro}-ros-base"),
        }
    }
}

/// Tools for building workspaces with `colcon build`.
const DEV_TOOLS: &[&str] = &[
    "colcon-common-extensions",
    "cmake",
    "ninja",
    "make",
    "pkg-config",
    "compilers",
    "rosdep",
];

pub struct Request {
    pub distro: String,
    pub variant: Variant,
    pub extra: Vec<String>,
    pub dev: bool,
    pub dry_run: bool,
}

/// What an install contains, saved as `ruckus.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Meta {
    pub distro: String,
    pub variant: Variant,
    pub specs: Vec<String>,
    pub channels: Vec<String>,
    pub platform: String,
    pub packages: usize,
    /// Seconds since the Unix epoch.
    pub installed: u64,
}

/// `$XDG_DATA_HOME/ruckus/distros`, else `~/.local/share/ruckus/distros`.
pub fn distros_dir() -> Result<PathBuf> {
    let base = match std::env::var_os("XDG_DATA_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => {
            PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".local/share")
        }
    };
    Ok(base.join("ruckus/distros"))
}

/// `jazzy`, `ros2_jazzy`, `ros2-jazzy` and `ros-jazzy` all name jazzy.
pub fn normalize(name: &str) -> Result<String> {
    let name = name.trim().to_ascii_lowercase();
    let distro = ["ros2_", "ros2-", "ros-"]
        .iter()
        .find_map(|prefix| name.strip_prefix(prefix))
        .unwrap_or(&name);
    if distro.is_empty() || !distro.chars().all(|c| c.is_ascii_lowercase()) {
        bail!("`{name}` is not a ROS distro name; try e.g. `jazzy` or `kilted`");
    }
    Ok(distro.to_string())
}

/// The directory of an installed distro, if there is one.
pub fn installed(distro: &str) -> Option<PathBuf> {
    let dir = distros_dir().ok()?.join(distro);
    dir.join("ruckus.toml").is_file().then_some(dir)
}

/// Installed distros, sorted by name.
pub fn list() -> Result<Vec<(String, Meta)>> {
    let dir = distros_dir()?;
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut distros = vec![];
    for entry in fs::read_dir(&dir).with_context(|| format!("cannot read {}", dir.display()))? {
        let path = entry?.path().join("ruckus.toml");
        if path.is_file() {
            let text = fs::read_to_string(&path)?;
            let meta: Meta =
                toml::from_str(&text).with_context(|| format!("invalid {}", path.display()))?;
            distros.push((meta.distro.clone(), meta));
        }
    }
    distros.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(distros)
}

pub fn specs(request: &Request) -> Vec<String> {
    let mut specs = vec![request.variant.package(&request.distro)];
    if request.dev {
        specs.extend(DEV_TOOLS.iter().map(|tool| tool.to_string()));
    }
    specs.extend(request.extra.iter().cloned());
    specs
}

/// Resolve, then (unless a dry run) download and install. Returns the distro directory.
pub fn install(request: &Request) -> Result<PathBuf> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(install_async(request))
}

async fn install_async(request: &Request) -> Result<PathBuf> {
    let distro = &request.distro;
    let dir = distros_dir()?.join(distro);
    let prefix = dir.join("prefix");
    let platform = Subdir::current().context("this platform has no conda packages")?;
    let channel_names = [format!("robostack-{distro}"), "conda-forge".to_string()];
    let config = ChannelConfig::default_with_root_dir(std::env::current_dir()?);
    let channels = channel_names
        .iter()
        .map(|name| Channel::from_str(name, &config))
        .collect::<Result<Vec<_>, _>>()?;
    let spec_names = specs(request);
    let specs = spec_names
        .iter()
        .map(|spec| MatchSpec::from_str(spec, ParseStrictness::Lenient))
        .collect::<Result<Vec<_>, _>>()?;

    let cache = rattler_cache::default_cache_dir()?;
    let package_cache = PackageCache::new(cache.join(rattler_cache::PACKAGE_CACHE_DIR));
    let gateway = Gateway::builder()
        .with_cache_dir(cache.join(rattler_cache::REPODATA_CACHE_DIR))
        .with_package_cache(package_cache.clone())
        .finish();

    let progress = spinner(format!("fetching package lists for {distro} ({platform})"));
    let repodata = gateway
        .query(channels, [platform, Subdir::NoArch], specs.clone())
        .recursive(true)
        .execute()
        .await;
    progress.finish_and_clear();
    let repodata = repodata.map_err(|err| match err {
        GatewayError::SubdirNotFoundError(_) => anyhow::anyhow!(
            "RoboStack has no ready-built ROS 2 `{distro}` for {platform}\n\
             hint: see https://robostack.github.io for the distros it has (e.g. jazzy, kilted)\n\
             hint: for a ROS you built from source, point an environment at it instead:\n  \
             ruckus env create {distro} --ros ~/ros2_{distro}/install"
        ),
        err => anyhow::Error::new(err).context("cannot fetch package lists"),
    })?;

    let progress = spinner("resolving packages".into());
    let virtual_packages = VirtualPackage::detect(&VirtualPackageOverrides::default(), None)?
        .into_iter()
        .map(GenericVirtualPackage::from)
        .collect::<Vec<_>>();
    let task = SolverTask {
        virtual_packages,
        specs,
        ..SolverTask::from_iter(repodata.iter().map(|data| data.iter()))
    };
    let solved = resolvo::Solver.solve(task);
    progress.finish_and_clear();
    let records = solved
        .map_err(|err| match err {
            SolveError::Unsolvable(reasons) => anyhow::anyhow!(
                "these packages cannot be installed together on {platform}:\n{}",
                reasons.join("\n")
            ),
            err => anyhow::Error::new(err).context("cannot resolve packages"),
        })?
        .records;

    print_summary(request, &records, &dir);
    if request.dry_run {
        return Ok(dir);
    }

    fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let packages = records.len();
    Installer::new()
        .with_package_cache(package_cache)
        .with_target_platform(platform)
        .with_reporter(IndicatifReporter::builder().clear_when_done(true).finish())
        .install(&prefix, records)
        .await
        .context("install failed; run the same command again to resume")?;

    write_activation(&prefix, &dir)?;
    let meta = Meta {
        distro: distro.clone(),
        variant: request.variant,
        specs: spec_names,
        channels: channel_names.to_vec(),
        platform: platform.to_string(),
        packages,
        installed: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs()),
    };
    fs::write(
        dir.join("ruckus.toml"),
        format!(
            "# Written by `ruckus install {distro}`.\n{}",
            toml::to_string(&meta)?
        ),
    )?;
    Ok(dir)
}

fn print_summary(request: &Request, records: &[RepoDataRecord], dir: &Path) {
    let bytes: u64 = records.iter().filter_map(|r| r.package_record.size).sum();
    let versions: BTreeMap<&str, String> = records
        .iter()
        .map(|r| {
            (
                r.package_record.name.as_normalized(),
                r.package_record.version.to_string(),
            )
        })
        .collect();
    let top = request.variant.package(&request.distro);
    eprintln!(
        "{} {} with Python {}: {} packages, {} to download (less if cached)",
        top,
        versions.get(top.as_str()).map_or("?", String::as_str),
        versions.get("python").map_or("?", String::as_str),
        records.len(),
        human_size(bytes),
    );
    eprintln!("into {}", dir.display());
}

/// Write `setup.{zsh,bash,sh}` that activate `prefix` like `conda activate`
/// would, and point colcon at this distro's own settings.
pub fn write_activation(prefix: &Path, dir: &Path) -> Result<()> {
    let platform = Subdir::current().context("this platform has no conda packages")?;
    let colcon = dir.join("colcon");
    fs::create_dir_all(&colcon)?;
    let defaults = colcon.join("defaults.yaml");
    if !defaults.exists() {
        fs::write(
            &defaults,
            "# colcon settings for this distro only (COLCON_HOME); see\n\
             # https://colcon.readthedocs.io/en/released/user/configuration.html\n{}\n",
        )?;
    }
    let colcon_home = format!(
        "export COLCON_HOME={}\n",
        quote(&colcon.display().to_string())
    );
    let zsh = activation_script(prefix, Zsh, platform)? + &colcon_home;
    let bash = activation_script(prefix, Bash::default(), platform)? + &colcon_home;
    fs::write(dir.join("setup.zsh"), zsh)?;
    // The bash script is plain POSIX `export` and `.` lines, so sh can use it too.
    fs::write(dir.join("setup.bash"), &bash)?;
    fs::write(dir.join("setup.sh"), &bash)?;
    Ok(())
}

fn activation_script<T: Shell + Clone + 'static>(
    prefix: &Path,
    shell: T,
    platform: Subdir,
) -> Result<String> {
    let result =
        Activator::from_path(prefix, shell, platform)?.activation(ActivationVariables {
            path_modification_behavior: PathModificationBehavior::Prepend,
            ..ActivationVariables::default()
        })?;
    Ok(format!(
        "# Loads the ROS installed by ruckus. Generated; do not edit.\n{}",
        result.script.contents()?
    ))
}

/// Delete an installed distro. Returns its directory.
pub fn uninstall(distro: &str) -> Result<PathBuf> {
    let dir = distros_dir()?.join(distro);
    // Only ever delete something that is recognisably a ruckus install.
    let ours = dir.join("ruckus.toml").is_file() || dir.join("prefix/conda-meta").is_dir();
    if !ours {
        let known = list()?
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>();
        if known.is_empty() {
            bail!("`{distro}` is not installed, and no distros are");
        }
        bail!(
            "`{distro}` is not installed (installed: {})",
            known.join(", ")
        );
    }
    fs::remove_dir_all(&dir).with_context(|| format!("cannot remove {}", dir.display()))?;
    Ok(dir)
}

fn spinner(message: String) -> ProgressBar {
    let spinner = ProgressBar::new_spinner().with_message(message);
    spinner.set_style(ProgressStyle::with_template("{spinner} {msg}").expect("valid template"));
    spinner.enable_steady_tick(Duration::from_millis(100));
    spinner
}

pub fn human_size(bytes: u64) -> String {
    let mb = bytes as f64 / 1_000_000.0;
    if mb >= 1000.0 {
        format!("{:.1} GB", mb / 1000.0)
    } else {
        format!("{mb:.0} MB")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_distro_names() {
        for name in ["jazzy", "ros2_jazzy", "ros2-jazzy", "ros-jazzy", " Jazzy "] {
            assert_eq!(normalize(name).unwrap(), "jazzy", "{name:?}");
        }
        for name in ["", "ros2_", "jazzy2", "../x", "a b"] {
            assert!(normalize(name).is_err(), "{name:?} should be rejected");
        }
    }

    #[test]
    fn specs_follow_the_request() {
        let mut request = Request {
            distro: "kilted".into(),
            variant: Variant::Base,
            extra: vec!["ros-kilted-turtlesim".into()],
            dev: false,
            dry_run: true,
        };
        assert_eq!(
            specs(&request),
            ["ros-kilted-ros-base", "ros-kilted-turtlesim"]
        );
        request.variant = Variant::Desktop;
        request.dev = true;
        let specs = specs(&request);
        assert_eq!(specs[0], "ros-kilted-desktop");
        assert!(specs.iter().any(|s| s == "colcon-common-extensions"));
    }

    #[test]
    fn activation_runs_the_prefix_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path().join("prefix");
        fs::create_dir_all(prefix.join("etc/conda/activate.d")).unwrap();
        fs::create_dir_all(prefix.join("bin")).unwrap();
        fs::write(
            prefix.join("etc/conda/activate.d/ros_activate.sh"),
            "export ROS_DISTRO=fake\n",
        )
        .unwrap();
        write_activation(&prefix, dir.path()).unwrap();
        assert!(dir.path().join("colcon/defaults.yaml").is_file());

        for ext in ["zsh", "bash", "sh"] {
            let script = fs::read_to_string(dir.path().join(format!("setup.{ext}"))).unwrap();
            assert!(script.contains("CONDA_PREFIX"), "{ext}: {script}");
            assert!(script.contains("ros_activate.sh"), "{ext}: {script}");
            assert!(script.contains("export COLCON_HOME="), "{ext}: {script}");
            assert!(
                script.contains(&prefix.join("bin").display().to_string()),
                "{ext}: {script}"
            );
        }
    }

    #[test]
    fn meta_round_trips() {
        let meta = Meta {
            distro: "jazzy".into(),
            variant: Variant::Desktop,
            specs: vec!["ros-jazzy-desktop".into()],
            channels: vec!["robostack-jazzy".into(), "conda-forge".into()],
            platform: "osx-arm64".into(),
            packages: 412,
            installed: 1_791_820_000,
        };
        let text = toml::to_string(&meta).unwrap();
        assert!(text.contains("variant = \"desktop\""), "{text}");
        assert_eq!(toml::from_str::<Meta>(&text).unwrap(), meta);
    }

    #[test]
    fn sizes_read_well() {
        assert_eq!(human_size(412_300_000), "412 MB");
        assert_eq!(human_size(2_345_000_000), "2.3 GB");
    }
}
