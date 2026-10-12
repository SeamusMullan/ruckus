//! `ruckus.lock`: the exact packages of an install.
//!
//! It uses the conda lock format of pixi (`pixi.lock`), read and written by
//! rattler_lock, with one environment and one platform. Each package is
//! recorded with its URL and hashes, so the same install can be repeated
//! later or on another machine with no solve.

use std::path::Path;

use anyhow::{Context, Result, bail};
use rattler_conda_types::{RepoDataRecord, Subdir};
use rattler_lock::{DEFAULT_ENVIRONMENT_NAME, LockFile, PlatformData};

pub const FILE: &str = "ruckus.lock";

/// What a lock file holds for one platform.
pub struct Locked {
    /// The ROS distro it is for, from its RoboStack channel.
    pub distro: Option<String>,
    pub records: Vec<RepoDataRecord>,
}

/// Lock file text for `records`, installed on `platform` from `channels` (URLs).
pub fn render(channels: &[String], platform: Subdir, records: &[RepoDataRecord]) -> Result<String> {
    let name = platform.to_string();
    let mut builder = LockFile::builder()
        .with_platforms(vec![PlatformData {
            name: name.as_str().try_into()?,
            subdir: platform,
            virtual_packages: vec![],
        }])?
        .with_channels(
            DEFAULT_ENVIRONMENT_NAME,
            channels.iter().map(String::as_str),
        );
    for record in records {
        builder.add_conda_package(DEFAULT_ENVIRONMENT_NAME, &name, record.clone().into())?;
    }
    Ok(builder.finish().render_to_string()?)
}

pub fn write(
    path: &Path,
    channels: &[String],
    platform: Subdir,
    records: &[RepoDataRecord],
) -> Result<()> {
    std::fs::write(path, render(channels, platform, records)?)
        .with_context(|| format!("cannot write {}", path.display()))
}

/// The records a lock file pins for `platform`.
pub fn read(path: &Path, platform: Subdir) -> Result<Locked> {
    let lock =
        LockFile::from_path(path).with_context(|| format!("cannot read {}", path.display()))?;
    let environment = match lock.default_environment() {
        Some(environment) => environment,
        None => {
            let mut environments = lock.environments();
            match (environments.next(), environments.next()) {
                (Some((_, environment)), None) => environment,
                (None, _) => bail!("{} has no environments", path.display()),
                _ => bail!(
                    "{} has several environments and none named `{DEFAULT_ENVIRONMENT_NAME}`",
                    path.display()
                ),
            }
        }
    };
    let records = lock
        .platform(platform.as_str())
        .map(|p| environment.conda_repodata_records(p))
        .transpose()
        .with_context(|| format!("invalid {}", path.display()))?
        .flatten();
    let Some(records) = records else {
        let mut has = environment
            .platforms()
            .map(|p| p.name().to_string())
            .collect::<Vec<_>>();
        has.sort();
        bail!(
            "{} has no packages for this platform, {platform} (it has: {})\n\
             hint: make a lock on {platform} with `ruckus install`, and use that",
            path.display(),
            if has.is_empty() {
                "none".into()
            } else {
                has.join(", ")
            }
        );
    };
    let distro = environment
        .channels()
        .iter()
        .find_map(|channel| {
            let last = channel.url.trim_end_matches('/').rsplit('/').next()?;
            last.strip_prefix("robostack-").map(str::to_string)
        })
        .or_else(|| {
            records.iter().find_map(|r| {
                let name = r.package_record.name.as_normalized();
                let distro = name.strip_prefix("ros-")?.strip_suffix("-ros-core")?;
                Some(distro.to_string())
            })
        });
    Ok(Locked { distro, records })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use rattler_conda_types::{PackageName, PackageRecord};

    pub(crate) fn record(name: &str, version: &str, depends: &[&str]) -> RepoDataRecord {
        let file = format!("{name}-{version}-h0_0.conda");
        let mut package_record = PackageRecord::new(
            PackageName::new_unchecked(name),
            version.parse::<rattler_conda_types::Version>().unwrap(),
            "h0_0".into(),
        );
        package_record.subdir = "noarch".into();
        package_record.depends = depends.iter().map(|d| d.to_string()).collect();
        RepoDataRecord {
            package_record,
            identifier: file.parse().unwrap(),
            url: format!("https://conda.anaconda.org/robostack-jazzy/noarch/{file}")
                .parse()
                .unwrap(),
            channel: Some("https://conda.anaconda.org/robostack-jazzy/".into()),
        }
    }

    #[test]
    fn round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        let platform = Subdir::current().unwrap();
        let records = vec![
            record("ros-jazzy-ros-base", "0.11.0", &["ros-jazzy-ros-core"]),
            record("ros-jazzy-ros-core", "0.11.0", &[]),
        ];
        let channels = [
            "https://conda.anaconda.org/robostack-jazzy/".to_string(),
            "https://conda.anaconda.org/conda-forge/".to_string(),
        ];
        write(&path, &channels, platform, &records).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        // The pixi.lock layout, so pixi and other rattler tools can read it.
        assert!(text.starts_with("version: "), "{text}");
        assert!(text.contains("environments:\n  default:"), "{text}");
        assert!(text.contains("robostack-jazzy/noarch/ros-jazzy-ros-core"));

        let locked = read(&path, platform).unwrap();
        assert_eq!(locked.distro.as_deref(), Some("jazzy"));
        let mut names = locked
            .records
            .iter()
            .map(|r| r.package_record.name.as_normalized().to_string())
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(names, ["ros-jazzy-ros-base", "ros-jazzy-ros-core"]);
        let base = locked
            .records
            .iter()
            .find(|r| r.package_record.name.as_normalized() == "ros-jazzy-ros-base")
            .unwrap();
        assert_eq!(base.url, records[0].url);
        assert_eq!(base.package_record.depends, ["ros-jazzy-ros-core"]);
    }

    #[test]
    fn other_platforms_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        let other = if Subdir::current() == Some(Subdir::Win64) {
            Subdir::Linux64
        } else {
            Subdir::Win64
        };
        let records = [record("ros-jazzy-ros-core", "0.11.0", &[])];
        write(&path, &[], other, &records).unwrap();
        let err = read(&path, Subdir::current().unwrap())
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("no packages for this platform"), "{err}");
        assert!(err.contains(&format!("it has: {other}")), "{err}");
        // With no channels, the distro comes from the package names.
        assert_eq!(read(&path, other).unwrap().distro.as_deref(), Some("jazzy"));
    }
}
