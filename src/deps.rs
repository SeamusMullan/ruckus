//! `ruckus deps`: install a workspace's dependencies, like `rosdep install`,
//! into a distro from `ruckus install`.
//!
//! Dependencies are the rosdep keys in each `package.xml`. ROS packages become
//! `ros-<distro>-<name>`. System keys such as `python3-numpy` or `eigen` are
//! looked up in RoboStack's rosdep-to-conda mapping, the `robostack.yaml` that
//! each RoboStack distro repository keeps. Copies of those are compiled in
//! from `src/robostack/` (MIT licensed, see the LICENSE there), so they always
//! match a release and need no download. Refresh them with
//! `sh src/robostack/refresh.sh`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rattler_conda_types::{Channel, ChannelConfig, MatchSpec, ParseStrictness, Subdir};
use rattler_repodata_gateway::Gateway;
use serde::Deserialize;

use crate::config::{Config, expand_tilde};
use crate::env::Env;
use crate::install;

pub struct Args {
    pub workspace: Option<PathBuf>,
    pub env: Option<String>,
    pub distro: Option<String>,
    pub dry_run: bool,
}

pub fn run(config: &Config, args: Args) -> Result<()> {
    let (distro, _) = target(config, args.env, args.distro)?;
    let dir = install::installed(&distro).expect("target returns installed distros");
    let workspace = match args.workspace {
        Some(path) => expand_tilde(&path.to_string_lossy()),
        None => std::env::current_dir()?,
    };

    let mut packages = vec![];
    for manifest in find_manifests(&workspace)? {
        let text = fs::read_to_string(&manifest)
            .with_context(|| format!("cannot read {}", manifest.display()))?;
        packages.push(parse(&text, &distro).with_context(|| format!("in {}", manifest.display()))?);
    }
    if packages.is_empty() {
        bail!(
            "no package.xml found in {}; run this in a workspace, or name one",
            workspace.display()
        );
    }
    println!(
        "{} packages in {}: {}",
        packages.len(),
        workspace.display(),
        packages
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );

    let keys = external_deps(&packages);
    let platform = Subdir::current().context("this platform has no conda packages")?;
    let mapping = Mapping::for_distro(&distro)?;
    let installed = installed_names(&dir.join("prefix"))?;
    let mut plan = Plan::new(&keys, &mapping, &distro, platform.as_str(), &installed);

    if !plan.ros.is_empty() {
        let names: Vec<String> = plan.ros.values().cloned().collect();
        eprintln!(
            "checking {} packages against robostack-{distro} ({platform})",
            names.len()
        );
        let available = available(&distro, platform, &names)?;
        plan.settle_ros(&available);
    }
    plan.report(&keys, platform.as_str());

    if plan.missing.is_empty() {
        if plan.resolved == 0 && !plan.unknown.is_empty() {
            bail!("none of the dependencies could be matched to RoboStack packages");
        }
        println!("all dependencies are installed");
        return Ok(());
    }
    if args.dry_run {
        println!("dry run; nothing installed");
        return Ok(());
    }
    // A re-install keeps the recorded variant, dev tools and earlier packages,
    // and their versions, so only the new packages change.
    let request = install::Request {
        distro: distro.clone(),
        extra: plan.missing.iter().cloned().collect(),
        ..Default::default()
    };
    install::install(&request)?;
    println!("installed {} packages into {distro}", plan.missing.len());
    Ok(())
}

/// The installed distro to add to: `--distro`, else the environment's ROS.
fn target(
    config: &Config,
    env: Option<String>,
    distro: Option<String>,
) -> Result<(String, install::Meta)> {
    let find =
        |distro: &str| install::list().map(|all| all.into_iter().find(|(name, _)| name == distro));
    if let Some(distro) = distro {
        let distro = install::normalize(&distro)?;
        return match find(&distro)? {
            Some(found) => Ok(found),
            None => {
                bail!("`{distro}` was not installed with `ruckus install` (see `ruckus distros`)")
            }
        };
    }
    let Some(name) = env.or_else(|| config.defaults.env.clone()) else {
        bail!("name a distro with --distro or an environment with -e, or set `env` in [defaults]");
    };
    let Some(ros) = Env::load(&name)?.ros_path() else {
        bail!("environment `{name}` has no ROS install");
    };
    // `ros` may be the distro directory or one of its setup files.
    let ros_dir = if ros.is_file() {
        ros.parent().map(Path::to_path_buf).unwrap_or_default()
    } else {
        ros.clone()
    };
    let ros_dir = ros_dir.canonicalize().unwrap_or(ros_dir);
    for (distro, meta) in install::list()? {
        let dir = install::installed(&distro);
        if dir.is_some_and(|dir| dir.canonicalize().unwrap_or(dir) == ros_dir) {
            return Ok((distro, meta));
        }
    }
    bail!(
        "environment `{name}` uses {}, which was not installed with `ruckus install`\n\
         hint: `ruckus deps` only works with distros installed by `ruckus install` for now; \
         use rosdep for a source build",
        ros.display()
    )
}

/// `package.xml` files under `dir`, the way colcon finds them.
pub fn find_manifests(dir: &Path) -> Result<Vec<PathBuf>> {
    if !dir.is_dir() {
        bail!("{} is not a directory", dir.display());
    }
    let mut found = vec![];
    let mut seen = BTreeSet::new();
    walk(dir, true, &mut seen, &mut found)?;
    found.sort();
    Ok(found)
}

fn walk(
    dir: &Path,
    top: bool,
    seen: &mut BTreeSet<PathBuf>,
    found: &mut Vec<PathBuf>,
) -> Result<()> {
    // Symlinked directories are followed, as colcon does, but each is visited once.
    if !seen.insert(dir.canonicalize()?) {
        return Ok(());
    }
    if ["COLCON_IGNORE", "AMENT_IGNORE", "CATKIN_IGNORE"]
        .iter()
        .any(|marker| dir.join(marker).exists())
    {
        return Ok(());
    }
    let manifest = dir.join("package.xml");
    if manifest.is_file() {
        // Packages do not nest.
        found.push(manifest);
        return Ok(());
    }
    for entry in fs::read_dir(dir).with_context(|| format!("cannot read {}", dir.display()))? {
        let path = entry?.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let skip = name.starts_with('.') || (top && ["build", "install", "log"].contains(&name));
        if !skip && path.is_dir() {
            walk(&path, false, seen, found)?;
        }
    }
    Ok(())
}

#[derive(Debug, PartialEq)]
pub struct Package {
    pub name: String,
    pub deps: BTreeSet<String>,
}

/// Every dependency tag except `doc_depend`. `run_depend` is format 1's `exec_depend`.
const DEP_TAGS: &[&str] = &[
    "depend",
    "build_depend",
    "build_export_depend",
    "buildtool_depend",
    "buildtool_export_depend",
    "exec_depend",
    "test_depend",
    "run_depend",
];

/// Read a `package.xml` (format 1, 2 or 3) for a ROS 2 `distro`.
pub fn parse(xml: &str, distro: &str) -> Result<Package> {
    let options = roxmltree::ParsingOptions {
        allow_dtd: true,
        ..Default::default()
    };
    let doc = roxmltree::Document::parse_with_options(xml, options)?;
    let root = doc.root_element();
    if root.tag_name().name() != "package" {
        bail!(
            "not a package manifest: the root element is <{}>",
            root.tag_name().name()
        );
    }
    let mut name = None;
    let mut deps = BTreeSet::new();
    for node in root.children().filter(|node| node.is_element()) {
        let tag = node.tag_name().name();
        let text = node.text().unwrap_or("").trim();
        if tag == "name" {
            name = Some(text.to_string());
        } else if DEP_TAGS.contains(&tag)
            && !text.is_empty()
            && node
                .attribute("condition")
                .is_none_or(|expr| condition(expr, distro))
        {
            deps.insert(text.to_string());
        }
    }
    match name {
        Some(name) if !name.is_empty() => Ok(Package { name, deps }),
        _ => bail!("the package has no <name>"),
    }
}

/// Evaluate a format 3 `condition`, such as `$ROS_VERSION == 2`, for ROS 2
/// on Python 3. Anything not understood (other variables, parentheses) counts
/// as true: installing a package too many beats missing one.
fn condition(expr: &str, distro: &str) -> bool {
    if expr.contains('(') {
        return true;
    }
    expr.split(" or ")
        .any(|all| all.split(" and ").all(|one| comparison(one, distro)))
}

fn comparison(text: &str, distro: &str) -> bool {
    let Some((op, lhs, rhs)) = ["==", "!=", ">=", "<=", ">", "<"]
        .iter()
        .find_map(|op| text.split_once(op).map(|(l, r)| (*op, l, r)))
    else {
        return true;
    };
    let value = |side: &str| {
        let side = side.trim().trim_matches(|c| c == '"' || c == '\'');
        match side.strip_prefix('$') {
            Some("ROS_VERSION") => Some("2".to_string()),
            Some("ROS_PYTHON_VERSION") => Some("3".to_string()),
            Some("ROS_DISTRO") => Some(distro.to_string()),
            Some(_) => None,
            None => Some(side.to_string()),
        }
    };
    let (Some(lhs), Some(rhs)) = (value(lhs), value(rhs)) else {
        return true;
    };
    let order = match (lhs.parse::<f64>(), rhs.parse::<f64>()) {
        (Ok(l), Ok(r)) => l.partial_cmp(&r),
        _ => Some(lhs.cmp(&rhs)),
    };
    let Some(order) = order else { return true };
    match op {
        "==" => order.is_eq(),
        "!=" => order.is_ne(),
        ">=" => order.is_ge(),
        "<=" => order.is_le(),
        ">" => order.is_gt(),
        _ => order.is_lt(),
    }
}

/// Dependencies on things outside the workspace, each with the packages that need it.
pub fn external_deps(packages: &[Package]) -> BTreeMap<String, BTreeSet<String>> {
    let local: BTreeSet<&str> = packages.iter().map(|p| p.name.as_str()).collect();
    let mut keys: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for package in packages {
        for dep in package.deps.iter().filter(|d| !local.contains(d.as_str())) {
            keys.entry(dep.clone())
                .or_default()
                .insert(package.name.clone());
        }
    }
    keys
}

/// RoboStack's rosdep keys and the conda packages that provide them.
pub struct Mapping(BTreeMap<String, Rule>);

#[derive(Deserialize)]
struct Rule {
    robostack: Option<Names>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Names {
    Everywhere(Vec<String>),
    /// Keyed by `linux`, `osx`, `win64`, or an exact platform such as `osx-arm64`.
    ByPlatform(BTreeMap<String, Vec<String>>),
}

#[derive(Debug, PartialEq)]
pub enum Resolution {
    /// Conda packages; none when this platform needs nothing for the key.
    System(Vec<String>),
    /// A system key RoboStack has nothing for on this platform.
    NotOnPlatform,
    /// Not a system key, so presumably a ROS package of this conda name.
    Ros(String),
}

impl Mapping {
    pub fn for_distro(distro: &str) -> Result<Self> {
        let text = match distro {
            "humble" => include_str!("robostack/humble.yaml"),
            "jazzy" => include_str!("robostack/jazzy.yaml"),
            "kilted" => include_str!("robostack/kilted.yaml"),
            "lyrical" => include_str!("robostack/lyrical.yaml"),
            _ => {
                eprintln!("note: no rosdep mapping for {distro}; using rolling's");
                include_str!("robostack/rolling.yaml")
            }
        };
        Self::parse(text)
    }

    fn parse(text: &str) -> Result<Self> {
        Ok(Self(
            serde_yaml::from_str(text).context("invalid rosdep mapping")?,
        ))
    }

    pub fn resolve(&self, key: &str, distro: &str, platform: &str) -> Resolution {
        match self.0.get(key).and_then(|rule| rule.robostack.as_ref()) {
            Some(Names::Everywhere(names)) => Resolution::System(names.clone()),
            Some(Names::ByPlatform(by)) => {
                let family = match platform.split('-').next().unwrap_or("") {
                    "win" => "win64",
                    family => family,
                };
                match by.get(platform).or_else(|| by.get(family)) {
                    Some(names) => Resolution::System(names.clone()),
                    None => Resolution::NotOnPlatform,
                }
            }
            None => Resolution::Ros(ros_package(distro, key)),
        }
    }
}

pub fn ros_package(distro: &str, key: &str) -> String {
    format!(
        "ros-{distro}-{}",
        key.replace('_', "-").to_ascii_lowercase()
    )
}

/// Names of the conda packages installed in `prefix`.
pub fn installed_names(prefix: &Path) -> Result<BTreeSet<String>> {
    #[derive(Deserialize)]
    struct Record {
        name: String,
    }
    let meta = prefix.join("conda-meta");
    let mut names = BTreeSet::new();
    if !meta.is_dir() {
        return Ok(names);
    }
    for entry in fs::read_dir(&meta).with_context(|| format!("cannot read {}", meta.display()))? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            let text = fs::read_to_string(&path)?;
            let record: Record = serde_json::from_str(&text)
                .with_context(|| format!("invalid {}", path.display()))?;
            names.insert(record.name);
        }
    }
    Ok(names)
}

/// What each dependency needs, sorted into what is there and what is not.
#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    /// Conda packages already installed.
    pub present: BTreeSet<String>,
    /// Conda packages to install.
    pub missing: BTreeSet<String>,
    /// Keys whose ROS package is not installed and still needs looking up, by key.
    pub ros: BTreeMap<String, String>,
    pub not_on_platform: BTreeSet<String>,
    pub unknown: BTreeSet<String>,
    /// Keys matched to conda packages, or that need none here.
    pub resolved: usize,
}

impl Plan {
    pub fn new(
        keys: &BTreeMap<String, BTreeSet<String>>,
        mapping: &Mapping,
        distro: &str,
        platform: &str,
        installed: &BTreeSet<String>,
    ) -> Self {
        let mut plan = Self::default();
        for key in keys.keys() {
            match mapping.resolve(key, distro, platform) {
                Resolution::System(names) => {
                    plan.resolved += 1;
                    for name in names {
                        if installed.contains(&name) {
                            plan.present.insert(name);
                        } else {
                            plan.missing.insert(name);
                        }
                    }
                }
                Resolution::NotOnPlatform => {
                    plan.not_on_platform.insert(key.clone());
                }
                Resolution::Ros(name) if installed.contains(&name) => {
                    plan.resolved += 1;
                    plan.present.insert(name);
                }
                Resolution::Ros(name) => {
                    plan.ros.insert(key.clone(), name);
                }
            }
        }
        plan
    }

    /// Sort the ROS packages still to look up by whether RoboStack has them.
    pub fn settle_ros(&mut self, available: &BTreeSet<String>) {
        for (key, name) in std::mem::take(&mut self.ros) {
            if available.contains(&name) {
                self.resolved += 1;
                self.missing.insert(name);
            } else {
                self.unknown.insert(key);
            }
        }
    }

    fn report(&self, keys: &BTreeMap<String, BTreeSet<String>>, platform: &str) {
        let list = |set: &BTreeSet<String>| set.iter().cloned().collect::<Vec<_>>().join(", ");
        let needed_by = |set: &BTreeSet<String>| {
            set.iter()
                .map(|key| {
                    let by = keys.get(key).map(list).unwrap_or_default();
                    format!("{key} (needed by {by})")
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        if !self.present.is_empty() {
            println!(
                "installed ({}): {}",
                self.present.len(),
                list(&self.present)
            );
        }
        if !self.missing.is_empty() {
            println!(
                "to install ({}): {}",
                self.missing.len(),
                list(&self.missing)
            );
        }
        if !self.not_on_platform.is_empty() {
            eprintln!(
                "warning: RoboStack has no package on {platform} for: {}",
                needed_by(&self.not_on_platform)
            );
        }
        if !self.unknown.is_empty() {
            eprintln!(
                "warning: no RoboStack package found for: {}",
                needed_by(&self.unknown)
            );
        }
    }
}

/// Which of `names` the distro's RoboStack channel has for this platform.
fn available(distro: &str, platform: Subdir, names: &[String]) -> Result<BTreeSet<String>> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let config = ChannelConfig::default_with_root_dir(std::env::current_dir()?);
            let channel = Channel::from_str(format!("robostack-{distro}"), &config)?;
            let specs = names
                .iter()
                .filter_map(|name| MatchSpec::from_str(name, ParseStrictness::Lenient).ok())
                .collect::<Vec<_>>();
            let cache = rattler_cache::default_cache_dir()?;
            let gateway = Gateway::builder()
                .with_cache_dir(cache.join(rattler_cache::REPODATA_CACHE_DIR))
                .finish();
            let repodata = gateway
                .query([channel], [platform, Subdir::NoArch], specs)
                .recursive(false)
                .execute()
                .await
                .context("cannot fetch package lists")?;
            Ok(repodata
                .iter()
                .flat_map(|data| data.iter())
                .map(|record| record.package_record.name.as_normalized().to_string())
                .collect())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_format_2() {
        let xml = r#"<?xml version="1.0"?>
<?xml-model href="http://download.ros.org/schema/package_format2.xsd" schematypens="http://www.w3.org/2001/XMLSchema"?>
<package format="2">
  <name>talker</name>
  <version>0.1.0</version>
  <buildtool_depend>ament_cmake</buildtool_depend>
  <depend>rclcpp</depend>
  <build_depend>eigen</build_depend>
  <build_export_depend>std_msgs</build_export_depend>
  <exec_depend>python3-numpy</exec_depend>
  <test_depend>ament_lint_auto</test_depend>
  <doc_depend>doxygen</doc_depend>
  <export><build_type>ament_cmake</build_type></export>
</package>"#;
        let package = parse(xml, "jazzy").unwrap();
        assert_eq!(package.name, "talker");
        assert_eq!(
            package.deps,
            set(&[
                "ament_cmake",
                "ament_lint_auto",
                "eigen",
                "python3-numpy",
                "rclcpp",
                "std_msgs"
            ])
        );
    }

    #[test]
    fn parses_format_3_conditions_and_format_1() {
        let xml = r#"<package format="3">
  <name>both</name>
  <buildtool_depend condition="$ROS_VERSION == 1">catkin</buildtool_depend>
  <buildtool_depend condition="$ROS_VERSION == 2">ament_python</buildtool_depend>
  <exec_depend condition="$ROS_PYTHON_VERSION == 2">python-yaml</exec_depend>
  <exec_depend condition="$ROS_PYTHON_VERSION == 3">python3-yaml</exec_depend>
  <exec_depend condition="$ROS_DISTRO != humble">new_thing</exec_depend>
  <exec_depend condition="$PLATFORM == mars">unknown_var</exec_depend>
</package>"#;
        assert_eq!(
            parse(xml, "humble").unwrap().deps,
            set(&["ament_python", "python3-yaml", "unknown_var"])
        );
        assert!(parse(xml, "jazzy").unwrap().deps.contains("new_thing"));

        let old = r#"<package><name>old</name><run_depend>roscpp</run_depend></package>"#;
        assert_eq!(parse(old, "jazzy").unwrap().deps, set(&["roscpp"]));
        assert!(parse("<package><version>1</version></package>", "jazzy").is_err());
        assert!(parse("<robot/>", "jazzy").is_err());
    }

    #[test]
    fn evaluates_conditions() {
        let cases = [
            ("$ROS_VERSION == 2", true),
            ("$ROS_VERSION == 1", false),
            ("$ROS_VERSION != 1", true),
            ("$ROS_VERSION >= 2 and $ROS_PYTHON_VERSION == 3", true),
            ("$ROS_VERSION == 1 or $ROS_PYTHON_VERSION == 3", true),
            ("$ROS_VERSION == 1 and $ROS_PYTHON_VERSION == 3", false),
            ("$ROS_DISTRO >= jazzy", true),
            ("$ROS_DISTRO == \"humble\"", false),
            ("$SOMETHING == 1", true),
            ("($ROS_VERSION == 1)", true),
        ];
        for (expr, expected) in cases {
            assert_eq!(condition(expr, "jazzy"), expected, "{expr}");
        }
    }

    #[test]
    fn finds_packages_like_colcon() {
        let ws = tempfile::tempdir().unwrap();
        let manifest = |rel: &str, name: &str, deps: &[&str]| {
            let dir = ws.path().join(rel);
            fs::create_dir_all(&dir).unwrap();
            let deps: String = deps
                .iter()
                .map(|d| format!("<depend>{d}</depend>"))
                .collect();
            fs::write(
                dir.join("package.xml"),
                format!("<package format=\"3\"><name>{name}</name>{deps}</package>"),
            )
            .unwrap();
        };
        manifest("src/a", "a", &["b", "rclcpp"]);
        manifest("src/group/b", "b", &["eigen", "rclcpp"]);
        manifest("src/a/nested", "nested", &["never"]);
        manifest("src/skipped", "skipped", &["never"]);
        fs::write(ws.path().join("src/skipped/COLCON_IGNORE"), "").unwrap();
        manifest("src/.hidden", "hidden", &["never"]);
        manifest("install/share/a", "a", &["never"]);
        manifest("build/a", "a", &["never"]);

        let found = find_manifests(ws.path()).unwrap();
        assert_eq!(
            found,
            [
                ws.path().join("src/a/package.xml"),
                ws.path().join("src/group/b/package.xml")
            ]
        );
        // Pointing at src/ finds the same packages.
        assert_eq!(find_manifests(&ws.path().join("src")).unwrap(), found);

        let packages: Vec<Package> = found
            .iter()
            .map(|path| parse(&fs::read_to_string(path).unwrap(), "jazzy").unwrap())
            .collect();
        let keys = external_deps(&packages);
        assert_eq!(
            keys.keys().cloned().collect::<BTreeSet<_>>(),
            set(&["eigen", "rclcpp"])
        );
        assert_eq!(keys["rclcpp"], set(&["a", "b"]));
    }

    #[test]
    fn vendored_mappings_parse() {
        for distro in ["humble", "jazzy", "kilted", "lyrical", "rolling"] {
            let mapping = Mapping::for_distro(distro).unwrap();
            assert!(mapping.0.len() > 400, "{distro}");
            assert_eq!(
                mapping.resolve("python3-numpy", distro, "linux-64"),
                Resolution::System(vec!["numpy".into()]),
                "{distro}"
            );
        }
    }

    #[test]
    fn maps_keys_to_conda_names() {
        let mapping = Mapping::for_distro("jazzy").unwrap();
        let resolve = |key, platform| mapping.resolve(key, "jazzy", platform);
        assert_eq!(
            resolve("python3-pytest", "osx-arm64"),
            Resolution::System(vec!["pytest".into()])
        );
        assert_eq!(
            resolve("acl", "linux-64"),
            Resolution::System(vec!["libacl".into()])
        );
        assert_eq!(resolve("acl", "osx-arm64"), Resolution::System(vec![]));
        assert_eq!(
            resolve("acl", "emscripten-wasm32"),
            Resolution::NotOnPlatform
        );
        // An exact platform beats its family.
        assert_eq!(resolve("netpbm", "osx-arm64"), Resolution::System(vec![]));
        assert_eq!(
            resolve("netpbm", "osx-64"),
            Resolution::System(vec!["netpbm".into()])
        );
        assert!(
            matches!(resolve("libopencv-dev", "win-64"), Resolution::System(names) if names.contains(&"libopencv".to_string()))
        );
        assert_eq!(
            resolve("nav2_msgs", "linux-64"),
            Resolution::Ros("ros-jazzy-nav2-msgs".into())
        );
    }

    #[test]
    fn plans_against_what_is_installed() {
        let mapping = Mapping::for_distro("jazzy").unwrap();
        let keys: BTreeMap<String, BTreeSet<String>> = [
            "rclcpp",
            "nav2_msgs",
            "made_up_pkg",
            "python3-numpy",
            "python3-pytest",
            "acl",
        ]
        .iter()
        .map(|key| (key.to_string(), set(&["a"])))
        .collect();
        let installed = set(&["ros-jazzy-rclcpp", "numpy", "python"]);
        let mut plan = Plan::new(&keys, &mapping, "jazzy", "osx-arm64", &installed);
        assert_eq!(plan.present, set(&["numpy", "ros-jazzy-rclcpp"]));
        assert_eq!(plan.missing, set(&["pytest"]));
        assert_eq!(
            plan.ros.values().cloned().collect::<BTreeSet<_>>(),
            set(&["ros-jazzy-made-up-pkg", "ros-jazzy-nav2-msgs"])
        );
        assert_eq!(plan.resolved, 4);

        plan.settle_ros(&set(&["ros-jazzy-nav2-msgs"]));
        assert!(plan.ros.is_empty());
        assert_eq!(plan.missing, set(&["pytest", "ros-jazzy-nav2-msgs"]));
        assert_eq!(plan.unknown, set(&["made_up_pkg"]));
        assert_eq!(plan.resolved, 5);
    }

    #[test]
    fn reads_installed_names() {
        let prefix = tempfile::tempdir().unwrap();
        assert!(installed_names(prefix.path()).unwrap().is_empty());
        let meta = prefix.path().join("conda-meta");
        fs::create_dir_all(&meta).unwrap();
        fs::write(
            meta.join("ros-jazzy-rclcpp-28.1.0-np2py312h1.json"),
            r#"{"name": "ros-jazzy-rclcpp", "version": "28.1.0"}"#,
        )
        .unwrap();
        fs::write(meta.join("history"), "").unwrap();
        assert_eq!(
            installed_names(prefix.path()).unwrap(),
            set(&["ros-jazzy-rclcpp"])
        );
    }
}
