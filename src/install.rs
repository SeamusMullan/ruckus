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
//! - `ruckus.lock`: the exact packages installed (see [`crate::lock`]), for
//!   `ruckus install <distro> --lock` to repeat the install elsewhere.
//! - `ruckus.toml`: what was asked for and installed, written last. A
//!   directory without it is an interrupted install; running `ruckus install`
//!   again finishes it.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use indicatif::{ProgressBar, ProgressStyle};
use rattler::install::{IndicatifReporter, Installer};
use rattler_cache::package_cache::PackageCache;
use rattler_conda_types::{
    Channel, ChannelConfig, GenericVirtualPackage, MatchSpec, ParseStrictness, PrefixRecord,
    RepoDataRecord, Subdir,
};
use rattler_repodata_gateway::{Gateway, GatewayError};
use rattler_shell::activation::{ActivationVariables, Activator, PathModificationBehavior};
use rattler_shell::shell::{Bash, Shell, Zsh};
use rattler_solve::{SolveError, SolverImpl, SolverTask, resolvo};
use rattler_virtual_packages::{VirtualPackage, VirtualPackageOverrides};
use serde::{Deserialize, Serialize};

use crate::lock;
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

/// What to install. Requests add up: the specs of earlier installs of the
/// distro are kept, so a package added once stays until it is removed.
#[derive(Debug, Default)]
pub struct Request {
    pub distro: String,
    /// `None` keeps the installed variant (desktop for a new install).
    pub variant: Option<Variant>,
    /// Package specs to add; one for an already requested package replaces it.
    pub extra: Vec<String>,
    /// Package names to stop requesting.
    pub remove: Vec<String>,
    /// Add colcon, cmake and compilers.
    pub dev: bool,
    /// Let every package move to its newest version. Otherwise installed
    /// packages keep their versions where they can.
    pub update: bool,
    /// Install exactly the packages in this lock file, with no solve.
    pub lock: Option<PathBuf>,
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

/// RoboStack's channel for a distro. RoboStack publishes every current distro
/// on prefix.dev; anaconda.org has only some of them (no Lyrical, for one).
pub fn robostack_channel(distro: &str) -> String {
    format!("https://prefix.dev/robostack-{distro}")
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

/// The `ruckus.toml` of a complete install in `dir`, if there is one.
fn read_meta(dir: &Path) -> Result<Option<Meta>> {
    let path = dir.join("ruckus.toml");
    if !path.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path)?;
    let meta = toml::from_str(&text).with_context(|| format!("invalid {}", path.display()))?;
    Ok(Some(meta))
}

/// The package a spec such as `cmake >=3.28` asks for.
fn spec_name(spec: &str) -> String {
    MatchSpec::from_str(spec, ParseStrictness::Lenient)
        .ok()
        .and_then(|spec| spec.name.as_exact().map(|n| n.as_normalized().to_string()))
        .unwrap_or_else(|| {
            let name = spec.split_whitespace().next().unwrap_or_default();
            name.to_ascii_lowercase()
        })
}

/// The variant and specs to solve for: the request merged into what an
/// earlier install of the distro (`previous`) asked for.
pub fn specs(request: &Request, previous: Option<&Meta>) -> Result<(Variant, Vec<String>)> {
    let distro = &request.distro;
    let mut specs = previous.map(|meta| meta.specs.clone()).unwrap_or_default();
    let variant = match (request.variant, previous) {
        (Some(variant), Some(meta)) if variant != meta.variant => {
            let old = meta.variant.package(distro);
            specs.retain(|spec| spec_name(spec) != old);
            variant
        }
        (Some(variant), _) => variant,
        (None, Some(meta)) => meta.variant,
        (None, None) => Variant::Desktop,
    };
    let top = variant.package(distro);
    if !specs.iter().any(|spec| spec_name(spec) == top) {
        specs.insert(0, top.clone());
    }
    // Defaults never replace a spec asked for earlier, such as `cmake 3.28.*`.
    if request.dev {
        for tool in DEV_TOOLS {
            if !specs.iter().any(|spec| spec_name(spec) == *tool) {
                specs.push(tool.to_string());
            }
        }
    }
    for extra in &request.extra {
        let name = spec_name(extra);
        match specs.iter_mut().find(|spec| spec_name(spec) == name) {
            Some(spec) => *spec = extra.clone(),
            None => specs.push(extra.clone()),
        }
    }
    for name in &request.remove {
        let name = spec_name(name);
        if name == top {
            bail!("`{top}` is the {variant:?} variant itself; use --variant to change it");
        }
        if !specs.iter().any(|spec| spec_name(spec) == name) {
            bail!(
                "`{name}` was not asked for, so it cannot be removed (asked for: {})",
                specs.join(", ")
            );
        }
        specs.retain(|spec| spec_name(spec) != name);
    }
    Ok((variant, specs))
}

/// The packages nothing else in `records` depends on: what was asked for.
fn top_level(records: &[RepoDataRecord]) -> Vec<String> {
    let needed: HashSet<String> = records
        .iter()
        .flat_map(|r| &r.package_record.depends)
        .map(|spec| spec_name(spec))
        .collect();
    let mut top: Vec<String> = records
        .iter()
        .map(|r| r.package_record.name.as_normalized().to_string())
        .filter(|name| !needed.contains(name))
        .collect();
    top.sort();
    top.dedup();
    top
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
    let channel_names = [robostack_channel(distro), "conda-forge".to_string()];
    let config = ChannelConfig::default_with_root_dir(std::env::current_dir()?);
    let channels = channel_names
        .iter()
        .map(|name| Channel::from_str(name, &config))
        .collect::<Result<Vec<_>, _>>()?;
    let previous = read_meta(&dir)?;
    // Read for an interrupted install too, so the packages it got are kept.
    let installed: Vec<PrefixRecord> = PrefixRecord::collect_from_prefix(&prefix)
        .with_context(|| format!("cannot read the packages in {}", prefix.display()))?;

    let cache = rattler_cache::default_cache_dir()?;
    let package_cache = PackageCache::new(cache.join(rattler_cache::PACKAGE_CACHE_DIR));
    let (variant, spec_names, records) = match &request.lock {
        Some(path) => from_lock(path, distro, platform)?,
        None => {
            let (variant, spec_names) = specs(request, previous.as_ref())?;
            let gateway = Gateway::builder()
                .with_cache_dir(cache.join(rattler_cache::REPODATA_CACHE_DIR))
                .with_package_cache(package_cache.clone())
                .finish();
            // Without this the solver picks the newest of everything, so adding
            // one package could upgrade the whole install.
            let locked: Vec<RepoDataRecord> = if request.update {
                vec![]
            } else {
                installed
                    .iter()
                    .map(|r| r.repodata_record.clone())
                    .collect()
            };
            let records = solve(&gateway, channels, platform, &spec_names, &locked).await?;
            (variant, spec_names, records)
        }
    };

    print_summary(&variant.package(distro), &records, &installed, &dir);
    if request.dry_run {
        return Ok(dir);
    }

    fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let packages = records.len();
    Installer::new()
        .with_package_cache(package_cache)
        .with_target_platform(platform)
        .with_installed_packages(installed)
        .with_reporter(IndicatifReporter::builder().clear_when_done(true).finish())
        .install(&prefix, records.clone())
        .await
        .context("install failed; run the same command again to resume")?;

    write_activation(&prefix, &dir)?;
    let channel_urls = channel_names
        .iter()
        .map(|name| {
            Ok(Channel::from_str(name, &config)?
                .base_url
                .as_str()
                .to_string())
        })
        .collect::<Result<Vec<_>>>()?;
    lock::write(&dir.join(lock::FILE), &channel_urls, platform, &records)?;
    let meta = Meta {
        distro: distro.clone(),
        variant,
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

/// The variant, specs and records of a lock file, checked to be for `distro`.
fn from_lock(
    path: &Path,
    distro: &str,
    platform: Subdir,
) -> Result<(Variant, Vec<String>, Vec<RepoDataRecord>)> {
    let locked = lock::read(path, platform)?;
    match locked.distro.as_deref() {
        Some(d) if d == distro => {}
        Some(d) => bail!(
            "{} is a lock for ROS 2 `{d}`, not `{distro}`\nhint: ruckus install {d} --lock {}",
            path.display(),
            path.display()
        ),
        None => bail!(
            "{} names no ROS distro, so it is not a lock from `ruckus install`",
            path.display()
        ),
    }
    let desktop = Variant::Desktop.package(distro);
    let has_desktop = locked
        .records
        .iter()
        .any(|r| r.package_record.name.as_normalized() == desktop);
    let variant = if has_desktop {
        Variant::Desktop
    } else {
        Variant::Base
    };
    // A lock does not record what was asked for, so later re-solves keep
    // what nothing else needs, which is what was asked for.
    Ok((variant, top_level(&locked.records), locked.records))
}

/// Resolve `spec_names`, preferring the `locked` records where they still fit.
async fn solve(
    gateway: &Gateway,
    channels: Vec<Channel>,
    platform: Subdir,
    spec_names: &[String],
    locked: &[RepoDataRecord],
) -> Result<Vec<RepoDataRecord>> {
    let specs = spec_names
        .iter()
        .map(|spec| MatchSpec::from_str(spec, ParseStrictness::Lenient))
        .collect::<Result<Vec<_>, _>>()?;
    let distro = channels
        .first()
        .and_then(|c| {
            let url = c.base_url.as_str().trim_end_matches('/');
            url.rsplit('/').next()?.strip_prefix("robostack-")
        })
        .unwrap_or("ROS")
        .to_string();

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
             hint: see https://robostack.github.io for the distros it has (e.g. humble, jazzy, kilted, lyrical)\n\
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
        locked_packages: locked.iter().collect(),
        ..SolverTask::from_iter(repodata.iter().map(|data| data.iter()))
    };
    let solved = resolvo::Solver.solve(task);
    progress.finish_and_clear();
    Ok(solved
        .map_err(|err| match err {
            SolveError::Unsolvable(reasons) => anyhow::anyhow!(
                "these packages cannot be installed together on {platform}:\n{}",
                reasons.join("\n")
            ),
            err => anyhow::Error::new(err).context("cannot resolve packages"),
        })?
        .records)
}

/// How many packages are new, change version or build, or go away.
fn changes(installed: &[RepoDataRecord], records: &[RepoDataRecord]) -> (usize, usize, usize) {
    let key = |r: &RepoDataRecord| {
        let p = &r.package_record;
        let name = p.name.as_normalized().to_string();
        (name, (p.version.to_string(), p.build.clone()))
    };
    let old: HashMap<_, _> = installed.iter().map(key).collect();
    let new: HashMap<_, _> = records.iter().map(key).collect();
    let added = new.keys().filter(|name| !old.contains_key(*name)).count();
    let removed = old.keys().filter(|name| !new.contains_key(*name)).count();
    let changed = new
        .iter()
        .filter(|(name, build)| old.get(*name).is_some_and(|old| old != *build))
        .count();
    (added, changed, removed)
}

fn print_summary(top: &str, records: &[RepoDataRecord], installed: &[PrefixRecord], dir: &Path) {
    // Only what is not in the prefix yet needs fetching; an add to an
    // existing install should not report the whole distro's size.
    let have: std::collections::HashSet<&str> = installed
        .iter()
        .map(|r| r.repodata_record.url.as_str())
        .collect();
    let bytes: u64 = records
        .iter()
        .filter(|r| !have.contains(r.url.as_str()))
        .filter_map(|r| r.package_record.size)
        .sum();
    let versions: BTreeMap<&str, String> = records
        .iter()
        .map(|r| {
            (
                r.package_record.name.as_normalized(),
                r.package_record.version.to_string(),
            )
        })
        .collect();
    eprintln!(
        "{} {} with Python {}: {} packages, {} to download (less if cached)",
        top,
        versions.get(top).map_or("?", String::as_str),
        versions.get("python").map_or("?", String::as_str),
        records.len(),
        human_size(bytes),
    );
    if !installed.is_empty() {
        let installed: Vec<RepoDataRecord> = installed
            .iter()
            .map(|r| r.repodata_record.clone())
            .collect();
        let (added, changed, removed) = changes(&installed, records);
        eprintln!("changes: {added} new, {changed} changed, {removed} removed");
    }
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
    } else if mb < 1.0 {
        format!("{:.0} kB", mb * 1000.0)
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

    fn meta(variant: Variant, specs: &[&str]) -> Meta {
        Meta {
            distro: "kilted".into(),
            variant,
            specs: specs.iter().map(|s| s.to_string()).collect(),
            channels: vec![],
            platform: "osx-arm64".into(),
            packages: 0,
            installed: 0,
        }
    }

    #[test]
    fn specs_follow_the_request() {
        let mut request = Request {
            distro: "kilted".into(),
            variant: Some(Variant::Base),
            extra: vec!["ros-kilted-turtlesim".into()],
            ..Request::default()
        };
        assert_eq!(
            specs(&request, None).unwrap(),
            (
                Variant::Base,
                vec!["ros-kilted-ros-base".into(), "ros-kilted-turtlesim".into()]
            )
        );
        request.variant = None;
        request.dev = true;
        let (variant, specs) = specs(&request, None).unwrap();
        assert_eq!(variant, Variant::Desktop);
        assert_eq!(specs[0], "ros-kilted-desktop");
        assert!(specs.iter().any(|s| s == "colcon-common-extensions"));
    }

    #[test]
    fn specs_keep_what_was_asked_for_before() {
        let before = meta(
            Variant::Base,
            &[
                "ros-kilted-ros-base",
                "cmake 3.28.*",
                "ros-kilted-turtlesim",
            ],
        );
        // A plain re-run keeps the added package, the variant, and the pinned cmake.
        let request = Request {
            distro: "kilted".into(),
            dev: true,
            ..Request::default()
        };
        let (variant, merged) = specs(&request, Some(&before)).unwrap();
        assert_eq!(variant, Variant::Base);
        assert_eq!(&merged[..3], before.specs.as_slice());
        assert!(merged.contains(&"ninja".to_string()));
        assert!(!merged.contains(&"cmake".to_string()));

        // A newer spec for the same package replaces the old one, in place.
        let request = Request {
            distro: "kilted".into(),
            extra: vec![
                "ROS-kilted-turtlesim >=1.8".into(),
                "ros-kilted-rviz2".into(),
            ],
            ..Request::default()
        };
        assert_eq!(
            specs(&request, Some(&before)).unwrap().1,
            [
                "ros-kilted-ros-base",
                "cmake 3.28.*",
                "ROS-kilted-turtlesim >=1.8",
                "ros-kilted-rviz2"
            ]
        );
    }

    #[test]
    fn specs_change_variant_and_remove_packages() {
        let before = meta(
            Variant::Base,
            &["ros-kilted-ros-base", "ros-kilted-turtlesim", "cmake"],
        );
        let request = Request {
            distro: "kilted".into(),
            variant: Some(Variant::Desktop),
            remove: vec!["ros-kilted-turtlesim".into()],
            ..Request::default()
        };
        assert_eq!(
            specs(&request, Some(&before)).unwrap(),
            (
                Variant::Desktop,
                vec!["ros-kilted-desktop".into(), "cmake".into()]
            )
        );

        let remove = |name: &str| Request {
            distro: "kilted".into(),
            remove: vec![name.into()],
            ..Request::default()
        };
        let err = specs(&remove("ros-kilted-ros-base"), Some(&before)).unwrap_err();
        assert!(err.to_string().contains("use --variant"), "{err}");
        let err = specs(&remove("ros-kilted-rviz2"), Some(&before)).unwrap_err();
        assert!(err.to_string().contains("was not asked for"), "{err}");
    }

    #[test]
    fn spec_names() {
        assert_eq!(spec_name("cmake"), "cmake");
        assert_eq!(spec_name("cmake >=3.28,<4"), "cmake");
        assert_eq!(spec_name("python_abi 3.12.* *_cp312"), "python_abi");
        assert_eq!(spec_name("conda-forge::Ninja"), "ninja");
    }

    #[test]
    fn top_level_is_what_nothing_needs() {
        let records = [
            lock::tests::record("ros-kilted-desktop", "0.12.0", &["ros-kilted-ros-base"]),
            lock::tests::record("ros-kilted-ros-base", "0.12.0", &["python >=3.12"]),
            lock::tests::record("python", "3.12.1", &[]),
            lock::tests::record("cmake", "3.30.0", &[]),
        ];
        assert_eq!(top_level(&records), ["cmake", "ros-kilted-desktop"]);
    }

    #[test]
    fn changes_are_counted() {
        let r = lock::tests::record;
        let old = [r("a", "1", &[]), r("b", "1", &[]), r("c", "1", &[])];
        let new = [r("a", "1", &[]), r("b", "2", &[]), r("d", "1", &[])];
        assert_eq!(changes(&old, &new), (1, 1, 1));
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
        assert_eq!(human_size(48_200), "48 kB");
    }
}
