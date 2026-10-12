//! `ruckus env`, `shell` and `run` against a fake ROS install and real shells.

use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;
use tempfile::TempDir;

/// A temporary HOME with a fake ROS install and a workspace in it.
struct Sandbox {
    home: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let ros = home.path().join("ros/install");
        std::fs::create_dir_all(&ros).unwrap();
        for ext in ["bash", "zsh"] {
            std::fs::write(
                ros.join(format!("setup.{ext}")),
                "export ROS_DISTRO=fake\nexport FAKE_SHELL_EXT=".to_string() + ext + "\n",
            )
            .unwrap();
        }
        let ws = home.path().join("ws/install");
        std::fs::create_dir_all(&ws).unwrap();
        for ext in ["bash", "zsh"] {
            std::fs::write(ws.join(format!("setup.{ext}")), "export FAKE_WS=yes\n").unwrap();
        }
        Self { home }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.home.path().join(rel)
    }

    fn ruckus(&self, shell: &str) -> assert_cmd::Command {
        let mut cmd = cargo_bin_cmd!("ruckus");
        cmd.env("HOME", self.home.path())
            .env("XDG_CONFIG_HOME", self.path(".config"))
            .env("XDG_CACHE_HOME", self.path(".cache"))
            .env("XDG_DATA_HOME", self.path(".local/share"))
            .env_remove("AMENT_PREFIX_PATH")
            .env_remove("COLCON_PREFIX_PATH")
            .env_remove("PYTHONPATH")
            .env("SHELL", shell)
            .env_remove("ZDOTDIR")
            .env_remove("RUCKUS_CONFIG")
            .env_remove("RUCKUS_ENV")
            .env_remove("ROS_DISTRO");
        cmd
    }

    fn create(&self, name: &str) {
        self.ruckus("/bin/bash")
            .args(["env", "create", name, "--ros"])
            .arg(self.path("ros/install"))
            .arg("--ws")
            .arg(self.path("ws"))
            .args(["--var", "ROS_DOMAIN_ID=7"])
            .args(["--pre", "export FAKE_PRE=$ROS_DOMAIN_ID-before"])
            .assert()
            .success();
    }
}

/// Absolute paths of the shells to test with, skipping any that are not installed.
fn shells() -> Vec<String> {
    ["bash", "zsh"]
        .iter()
        .filter_map(|name| {
            let out = Command::new("sh")
                .args(["-c", &format!("command -v {name}")])
                .output()
                .ok()?;
            let path = String::from_utf8(out.stdout).ok()?.trim().to_string();
            Path::new(&path).is_file().then_some(path)
        })
        .collect()
}

#[test]
fn create_list_show_remove() {
    let sb = Sandbox::new();
    let unbuilt = sb.path("unbuilt");
    std::fs::create_dir(&unbuilt).unwrap();

    sb.ruckus("/bin/bash")
        .args(["env", "ls"])
        .assert()
        .success()
        .stdout(predicate::str::contains("no environments yet"));

    sb.ruckus("/bin/bash")
        .args(["env", "create", "lab", "--ros"])
        .arg(sb.path("ros/install"))
        .arg("--ws")
        .arg(sb.path("ws"))
        .arg("--ws")
        .arg(&unbuilt)
        .args(["--var", "ROS_DOMAIN_ID=7"])
        .assert()
        .success()
        .stdout(predicate::str::contains("created lab"))
        .stderr(predicate::str::contains("unbuilt is not built yet"));

    // Paths under HOME are stored as ~/..., so the file stays readable and portable.
    let file = std::fs::read_to_string(sb.path(".config/ruckus/envs/lab.toml")).unwrap();
    assert!(file.contains("ros = \"~/ros/install\""), "{file}");
    assert!(
        file.contains("workspaces = [\"~/ws\", \"~/unbuilt\"]"),
        "{file}"
    );

    sb.ruckus("/bin/bash")
        .args(["env", "ls"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "lab  ~/ros/install + ~/ws + ~/unbuilt",
        ));

    sb.ruckus("/bin/bash")
        .args(["env", "show", "lab"])
        .assert()
        .success()
        .stdout(predicate::str::contains("ros:       ~/ros/install  (ok, "))
        .stdout(predicate::str::contains("workspace: ~/ws  (built)"))
        .stdout(predicate::str::contains(
            "workspace: ~/unbuilt  (not built)",
        ))
        .stdout(predicate::str::contains("var:       ROS_DOMAIN_ID=7"));

    sb.ruckus("/bin/bash")
        .args(["env", "create", "lab", "--ros"])
        .arg(sb.path("ros/install"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));
    sb.ruckus("/bin/bash")
        .args(["env", "create", "lab", "--force", "--ros"])
        .arg(sb.path("ros/install"))
        .assert()
        .success();

    sb.ruckus("/bin/bash")
        .args(["env", "rm", "lab"])
        .assert()
        .success();
    sb.ruckus("/bin/bash")
        .args(["env", "show", "lab"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no environment named `lab`"));
    // The ROS install itself is never touched.
    assert!(sb.path("ros/install/setup.bash").is_file());
}

#[test]
fn create_rejects_bad_input() {
    let sb = Sandbox::new();
    sb.ruckus("/bin/bash")
        .args(["env", "create", "x", "--ros"])
        .arg(sb.path("ws"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("is not a ROS install"));
    sb.ruckus("/bin/bash")
        .args(["env", "create", "x", "--ros"])
        .arg(sb.path("ros/install"))
        .arg("--ws")
        .arg(sb.path("nope"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("does not exist"));
    sb.ruckus("/bin/bash")
        .args(["env", "create", "a/b", "--ros"])
        .arg(sb.path("ros/install"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("environment names use"));
    sb.ruckus("/bin/bash")
        .args(["env", "create", "x", "--var", "novalue", "--ros"])
        .arg(sb.path("ros/install"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("expected NAME=VALUE"));
}

#[test]
fn run_loads_env_in_each_shell() {
    let sb = Sandbox::new();
    sb.create("lab");
    for shell in shells() {
        let ext = Path::new(&shell)
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        sb.ruckus(&shell)
            .args(["run", "-e", "lab", "--", "sh", "-c"])
            .arg("echo $ROS_DISTRO $FAKE_SHELL_EXT $FAKE_WS $ROS_DOMAIN_ID $FAKE_PRE")
            .assert()
            .success()
            // `pre` runs first, before ROS_DOMAIN_ID is exported.
            .stdout(format!("fake {ext} yes 7 -before\n"));
    }
}

#[test]
fn run_passes_exit_code_and_uses_default_env() {
    let sb = Sandbox::new();
    sb.create("lab");
    sb.ruckus("/bin/bash")
        .args(["run", "-e", "lab", "--", "sh", "-c", "exit 3"])
        .assert()
        .code(3);

    sb.ruckus("/bin/bash")
        .args(["run", "--", "true"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("name an environment"));

    std::fs::write(
        sb.path(".config/ruckus/config.toml"),
        "[defaults]\nenv = \"lab\"\n",
    )
    .unwrap();
    sb.ruckus("/bin/bash")
        .args(["run", "--", "printenv", "ROS_DISTRO"])
        .assert()
        .success()
        .stdout("fake\n");
    sb.ruckus("/bin/bash")
        .args(["env", "ls"])
        .assert()
        .success()
        .stdout(predicate::str::starts_with("* lab"));
}

#[test]
fn shell_keeps_user_rc_and_loads_env() {
    let sb = Sandbox::new();
    sb.create("lab");
    std::fs::write(sb.path(".bashrc"), "export USER_RC=bash\n").unwrap();
    std::fs::write(sb.path(".zshrc"), "export USER_RC=zsh\n").unwrap();
    std::fs::write(sb.path(".zshenv"), "export USER_ZSHENV=yes\n").unwrap();

    for shell in shells() {
        let ext = Path::new(&shell)
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        let mut expected = format!("RESULT fake {ext} yes 7 lab {ext}");
        if ext == "zsh" {
            // .zshenv ran, and $ZDOTDIR is handed back to the user's own directory.
            expected.push_str(&format!(" yes {}", sb.home.path().display()));
        }
        let probe = if ext == "zsh" {
            "echo RESULT $ROS_DISTRO $FAKE_SHELL_EXT $FAKE_WS $ROS_DOMAIN_ID $RUCKUS_ENV $USER_RC $USER_ZSHENV $ZDOTDIR\nexit\n"
        } else {
            "echo RESULT $ROS_DISTRO $FAKE_SHELL_EXT $FAKE_WS $ROS_DOMAIN_ID $RUCKUS_ENV $USER_RC\nexit\n"
        };
        sb.ruckus(&shell)
            .args(["shell", "lab"])
            .write_stdin(probe)
            .assert()
            .success()
            .stdout(predicate::str::contains(expected))
            .stderr(predicate::str::contains("ruckus: entering lab"));
    }
}

#[test]
fn up_uses_env() {
    let sb = Sandbox::new();
    sb.create("lab");
    std::fs::write(
        sb.path(".config/ruckus/config.toml"),
        "[layouts.demo]\npanes = [{ run = \"ros2 topic list\" }]\n",
    )
    .unwrap();
    let ws = sb.path("ws");
    sb.ruckus("/bin/bash")
        .args(["up", "demo", "--dry-run", "-e", "lab"])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "source {}",
            sb.path("ros/install/setup.bash").display()
        )))
        .stdout(predicate::str::contains(format!(
            "source {}",
            ws.join("install/setup.bash").display()
        )))
        .stdout(predicate::str::contains("export ROS_DOMAIN_ID=7"))
        .stdout(predicate::str::contains(format!("-c {}", ws.display())));
    sb.ruckus("/bin/bash")
        .args(["up", "demo", "--dry-run", "-e", "nope"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("available: lab"));
}

#[test]
fn loading_ros_unloads_the_shells_old_ros() {
    let sb = Sandbox::new();
    sb.create("lab");
    let old = sb.path("old_ws/install/oldpkg");
    std::fs::create_dir_all(old.join("bin")).unwrap();
    let path = format!(
        "{}:{}",
        old.join("bin").display(),
        std::env::var("PATH").unwrap()
    );
    for shell in shells() {
        sb.ruckus(&shell)
            .env("ROS_DISTRO", "old")
            .env("ROS_VERSION", "2")
            .env("AMENT_PREFIX_PATH", &old)
            .env("PYTHONPATH", sb.path("old_ws/build/oldpkg"))
            .env("PATH", &path)
            .args(["run", "-e", "lab", "--", "sh", "-c"])
            .arg("echo $ROS_DISTRO ${ROS_VERSION:-unset} ${PYTHONPATH:-unset}; echo $PATH")
            .assert()
            .success()
            .stdout(predicate::str::starts_with("fake unset unset\n"))
            .stdout(predicate::str::contains("old_ws").not());
    }
}

#[test]
fn installed_distros_are_listed_named_and_removed() {
    let sb = Sandbox::new();
    sb.ruckus("/bin/bash")
        .arg("distros")
        .assert()
        .success()
        .stdout(predicate::str::contains("no distros installed yet"));

    // What `ruckus install` leaves behind, without the 1 GB download.
    let dir = sb.path(".local/share/ruckus/distros/fakey");
    std::fs::create_dir_all(dir.join("prefix/conda-meta")).unwrap();
    std::fs::write(dir.join("setup.bash"), "export ROS_DISTRO=fakey\n").unwrap();
    std::fs::write(
        dir.join("ruckus.toml"),
        "distro = \"fakey\"\nvariant = \"base\"\nspecs = [\"ros-fakey-ros-base\"]\n\
         channels = [\"robostack-fakey\"]\nplatform = \"osx-arm64\"\npackages = 3\ninstalled = 0\n",
    )
    .unwrap();

    sb.ruckus("/bin/bash")
        .arg("distros")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "fakey  Base (3 packages, osx-arm64)",
        ));

    // `--ros` takes an installed distro by name; `ros2_` is accepted as in `ruckus install`.
    sb.ruckus("/bin/bash")
        .args(["env", "create", "f", "--ros", "ros2_fakey"])
        .assert()
        .success();
    sb.ruckus("/bin/bash")
        .args(["run", "-e", "f", "--", "printenv", "ROS_DISTRO"])
        .assert()
        .success()
        .stdout("fakey\n");
    sb.ruckus("/bin/bash")
        .args(["env", "create", "g", "--ros", "nosuchdistro"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "neither a path nor an installed distro",
        ));

    sb.ruckus("/bin/bash")
        .args(["uninstall", "fakey"])
        .assert()
        .success()
        .stderr(predicate::str::contains("environment `f` used it"));
    assert!(!dir.exists());
    sb.ruckus("/bin/bash")
        .args(["uninstall", "fakey"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not installed"));
}

#[test]
fn install_rejects_bad_names_before_any_download() {
    let sb = Sandbox::new();
    sb.ruckus("/bin/bash")
        .args(["install", "jazzy2"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not a ROS distro name"));
}

/// The conda name of this platform, as lock files use it.
fn platform() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "osx-arm64",
        ("macos", _) => "osx-64",
        ("linux", "aarch64") => "linux-aarch64",
        _ => "linux-64",
    }
}

/// A ruckus.lock (pixi.lock format) with one tiny real package: an old tzdata.
fn tzdata_lock(channel: &str, platform: &str) -> String {
    let url = "https://conda.anaconda.org/conda-forge/noarch/tzdata-2024a-h8827d51_1.conda";
    format!(
        "version: 7\nplatforms:\n- name: {platform}\nenvironments:\n  default:\n    channels:\n    \
         - url: https://conda.anaconda.org/{channel}/\n    \
         - url: https://conda.anaconda.org/conda-forge/\n    packages:\n      {platform}:\n      \
         - conda: {url}\npackages:\n- conda: {url}\n  sha256: \
         7d21c95f61319dba9209ca17d1935e6128af4235a67ee4e57a00908a1450081e\n  \
         md5: 8bfdead4e0fff0383ae4c9c50d0531bd\n  license: LicenseRef-Public-Domain\n  \
         size: 124164\n  timestamp: 1724736371498\n"
    )
}

#[test]
fn install_checks_lock_files_and_removals_before_any_download() {
    let sb = Sandbox::new();
    let lock = sb.path("ruckus.lock");
    std::fs::write(&lock, tzdata_lock("robostack-kilted", platform())).unwrap();
    sb.ruckus("/bin/bash")
        .args(["install", "jazzy", "--lock"])
        .arg(&lock)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "is a lock for ROS 2 `kilted`, not `jazzy`",
        ));
    std::fs::write(&lock, tzdata_lock("robostack-jazzy", "win-64")).unwrap();
    sb.ruckus("/bin/bash")
        .args(["install", "jazzy", "--lock"])
        .arg(&lock)
        .assert()
        .failure()
        .stderr(predicate::str::contains(format!(
            "no packages for this platform, {} (it has: win-64)",
            platform()
        )));
    sb.ruckus("/bin/bash")
        .args(["install", "jazzy", "--add", "x", "--lock"])
        .arg(&lock)
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));

    let dir = sb.path(".local/share/ruckus/distros/fakey");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("ruckus.toml"),
        "distro = \"fakey\"\nvariant = \"base\"\nspecs = [\"ros-fakey-ros-base\", \"cmake\"]\n\
         channels = [\"robostack-fakey\"]\nplatform = \"osx-arm64\"\npackages = 3\ninstalled = 0\n",
    )
    .unwrap();
    sb.ruckus("/bin/bash")
        .args(["install", "fakey", "--remove", "ros-fakey-turtlesim"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "`ros-fakey-turtlesim` was not asked for, so it cannot be removed \
             (asked for: ros-fakey-ros-base, cmake",
        ));
    sb.ruckus("/bin/bash")
        .args(["install", "fakey", "--remove", "ros-fakey-ros-base"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("use --variant"));
}

/// Installs one tiny real package from a lock, then repeats the install from
/// the lock that wrote. Needs the network.
#[test]
fn install_from_lock_is_exact_and_repeatable() {
    if std::env::var_os("RUCKUS_NETWORK_TESTS").is_none() {
        eprintln!("skipping: set RUCKUS_NETWORK_TESTS=1 to run");
        return;
    }
    let sb = Sandbox::new();
    let lock = sb.path("shared.lock");
    std::fs::write(&lock, tzdata_lock("robostack-jazzy", platform())).unwrap();
    sb.ruckus("/bin/bash")
        .args(["install", "ros2_jazzy", "--no-env", "--lock"])
        .arg(&lock)
        .assert()
        .success()
        .stderr(predicate::str::contains("1 packages"));

    let dir = sb.path(".local/share/ruckus/distros/jazzy");
    assert!(
        dir.join("prefix/conda-meta/tzdata-2024a-h8827d51_1.json")
            .is_file()
    );
    let meta = std::fs::read_to_string(dir.join("ruckus.toml")).unwrap();
    assert!(meta.contains("variant = \"base\""), "{meta}");
    assert!(meta.contains("specs = [\"tzdata\"]"), "{meta}");
    let written = std::fs::read_to_string(dir.join("ruckus.lock")).unwrap();
    assert!(
        written.contains("tzdata-2024a-h8827d51_1.conda"),
        "{written}"
    );
    assert!(written.contains("robostack-jazzy"), "{written}");
    sb.ruckus("/bin/bash")
        .arg("distros")
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "lock: {}",
            dir.join("ruckus.lock").display()
        )));

    // A re-solve keeps the installed (old) tzdata; --update moves it on.
    sb.ruckus("/bin/bash")
        .args(["install", "jazzy", "--no-dev", "--dry-run"])
        .assert()
        .success()
        .stderr(predicate::str::contains("ros-jazzy-ros-base"))
        .stderr(predicate::str::contains("0 changed, 0 removed"));
    sb.ruckus("/bin/bash")
        .args(["install", "jazzy", "--no-dev", "--dry-run", "--update"])
        .assert()
        .success()
        .stderr(predicate::str::contains("1 changed, 0 removed"));

    // Someone else repeats the install from the lock this one wrote.
    let other = Sandbox::new();
    other
        .ruckus("/bin/bash")
        .args(["install", "jazzy", "--no-env", "--lock"])
        .arg(dir.join("ruckus.lock"))
        .assert()
        .success();
    assert_eq!(
        std::fs::read_to_string(other.path(".local/share/ruckus/distros/jazzy/ruckus.lock"))
            .unwrap(),
        written
    );
}

/// Resolves real RoboStack package lists (no install). Needs the network, so
/// it runs only when RUCKUS_NETWORK_TESTS is set, as it is in CI.
#[test]
fn install_dry_run_resolves_from_robostack() {
    if std::env::var_os("RUCKUS_NETWORK_TESTS").is_none() {
        eprintln!("skipping: set RUCKUS_NETWORK_TESTS=1 to run");
        return;
    }
    let sb = Sandbox::new();
    sb.ruckus("/bin/bash")
        .args([
            "install",
            "jazzy",
            "--variant",
            "base",
            "--no-dev",
            "--dry-run",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("ros-jazzy-ros-base"))
        .stderr(predicate::str::contains("with Python 3."));
    sb.ruckus("/bin/bash")
        .args(["install", "lyrical", "--dry-run"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "RoboStack has no ready-built ROS 2 `lyrical`",
        ));
}
