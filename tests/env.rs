//! `ruckus env`, `shell` and `run` against a fake ROS install and real shells.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

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
        assert_cmd::Command::from_std(self.command(env!("CARGO_BIN_EXE_ruckus"), shell))
    }

    /// `program` with this HOME and its XDG dirs, and no ROS or ruckus settings from outside.
    fn command(&self, program: impl AsRef<OsStr>, shell: &str) -> Command {
        let mut cmd = Command::new(program);
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

/// A distro laid out like `ruckus install` leaves it, with these conda packages "installed".
fn fake_install(sb: &Sandbox, distro: &str, packages: &[&str]) {
    let dir = sb.path(&format!(".local/share/ruckus/distros/{distro}"));
    let meta = dir.join("prefix/conda-meta");
    std::fs::create_dir_all(&meta).unwrap();
    for name in packages {
        std::fs::write(
            meta.join(format!("{name}-1.0-0.json")),
            format!("{{\"name\": \"{name}\", \"version\": \"1.0\"}}"),
        )
        .unwrap();
    }
    std::fs::write(
        dir.join("setup.bash"),
        format!("export ROS_DISTRO={distro}\n"),
    )
    .unwrap();
    std::fs::write(
        dir.join("ruckus.toml"),
        format!(
            "distro = \"{distro}\"\nvariant = \"base\"\nspecs = [\"ros-{distro}-ros-base\"]\n\
             channels = [\"robostack-{distro}\"]\nplatform = \"osx-arm64\"\npackages = 3\ninstalled = 0\n"
        ),
    )
    .unwrap();
}

/// Write `<ws>/src/<name>/package.xml` with these dependency tags.
fn fake_package(ws: &Path, name: &str, deps: &str) {
    let dir = ws.join("src").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("package.xml"),
        format!("<?xml version=\"1.0\"?>\n<package format=\"3\">\n  <name>{name}</name>\n{deps}\n</package>\n"),
    )
    .unwrap();
}

#[test]
fn deps_reports_what_a_workspace_needs() {
    let sb = Sandbox::new();
    fake_install(
        &sb,
        "jazzy",
        &[
            "ros-jazzy-rclcpp",
            "ros-jazzy-ament-cmake",
            "numpy",
            "python",
        ],
    );
    let ws = sb.path("app_ws");
    fake_package(
        &ws,
        "app",
        "<buildtool_depend>ament_cmake</buildtool_depend>\n<depend>rclcpp</depend>\n\
         <depend>app_msgs</depend>\n<exec_depend>python3-numpy</exec_depend>\n\
         <test_depend>python3-pytest</test_depend>\n<doc_depend>made_up_doc_tool</doc_depend>\n\
         <exec_depend condition=\"$ROS_VERSION == 1\">roscpp</exec_depend>",
    );
    fake_package(&ws, "app_msgs", "<depend>rclcpp</depend>");
    fake_package(&ws, "ignored", "<depend>made_up_pkg</depend>");
    std::fs::write(ws.join("src/ignored/COLCON_IGNORE"), "").unwrap();
    sb.ruckus("/bin/bash")
        .args(["env", "create", "j", "--ros", "jazzy"])
        .assert()
        .success();

    // Everything ROS is installed, so this needs no network.
    sb.ruckus("/bin/bash")
        .args(["deps", "-e", "j", "--dry-run"])
        .arg(&ws)
        .assert()
        .success()
        .stdout(predicate::str::contains("2 packages in"))
        .stdout(predicate::str::contains(
            "installed (3): numpy, ros-jazzy-ament-cmake, ros-jazzy-rclcpp",
        ))
        .stdout(predicate::str::contains("to install (1): pytest"))
        .stdout(predicate::str::contains("dry run; nothing installed"));

    // From inside the workspace, with --distro or the default environment.
    sb.ruckus("/bin/bash")
        .current_dir(&ws)
        .args(["deps", "--distro", "ros2_jazzy", "-n"])
        .assert()
        .success()
        .stdout(predicate::str::contains("to install (1): pytest"));
    std::fs::write(
        sb.path(".config/ruckus/config.toml"),
        "[defaults]\nenv = \"j\"\n",
    )
    .unwrap();
    sb.ruckus("/bin/bash")
        .current_dir(ws.join("src"))
        .args(["deps", "-n"])
        .assert()
        .success()
        .stdout(predicate::str::contains("to install (1): pytest"));

    // Nothing missing: nothing to do, even without --dry-run.
    fake_install(
        &sb,
        "jazzy",
        &[
            "ros-jazzy-rclcpp",
            "ros-jazzy-ament-cmake",
            "numpy",
            "pytest",
        ],
    );
    sb.ruckus("/bin/bash")
        .arg("deps")
        .arg(&ws)
        .assert()
        .success()
        .stdout(predicate::str::contains("all dependencies are installed"));
}

#[test]
fn deps_needs_a_ruckus_installed_distro() {
    let sb = Sandbox::new();
    sb.create("lab");
    let ws = sb.path("app_ws");
    fake_package(&ws, "app", "<depend>rclcpp</depend>");
    sb.ruckus("/bin/bash")
        .args(["deps", "-e", "lab", "-n"])
        .arg(&ws)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "only works with distros installed by `ruckus install`",
        ));
    sb.ruckus("/bin/bash")
        .args(["deps", "--distro", "jazzy", "-n"])
        .arg(&ws)
        .assert()
        .failure()
        .stderr(predicate::str::contains("`jazzy` was not installed"));
    sb.ruckus("/bin/bash")
        .args(["deps", "-n"])
        .arg(&ws)
        .assert()
        .failure()
        .stderr(predicate::str::contains("name a distro with --distro"));

    fake_install(&sb, "jazzy", &[]);
    sb.ruckus("/bin/bash")
        .args(["deps", "--distro", "jazzy", "-n"])
        .arg(sb.path("ws"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("no package.xml found"));
}

/// Looks real ROS packages up in RoboStack's jazzy channel (no install).
/// Needs the network, so it runs only when RUCKUS_NETWORK_TESTS is set.
#[test]
fn deps_dry_run_resolves_from_robostack() {
    if std::env::var_os("RUCKUS_NETWORK_TESTS").is_none() {
        eprintln!("skipping: set RUCKUS_NETWORK_TESTS=1 to run");
        return;
    }
    let sb = Sandbox::new();
    fake_install(&sb, "jazzy", &["ros-jazzy-rclcpp"]);
    let ws = sb.path("nav_ws");
    fake_package(
        &ws,
        "nav_app",
        "<depend>rclcpp</depend>\n<depend>nav2_msgs</depend>\n<depend>tf2_ros</depend>\n\
         <exec_depend>python3-numpy</exec_depend>\n<depend>made_up_pkg_xyz</depend>",
    );
    sb.ruckus("/bin/bash")
        .args(["deps", "--distro", "jazzy", "--dry-run"])
        .arg(&ws)
        .assert()
        .success()
        .stdout(predicate::str::contains("installed (1): ros-jazzy-rclcpp"))
        .stdout(predicate::str::contains(
            "to install (3): numpy, ros-jazzy-nav2-msgs, ros-jazzy-tf2-ros",
        ))
        .stderr(predicate::str::contains(
            "no RoboStack package found for: made_up_pkg_xyz (needed by nav_app)",
        ));

    let ws = sb.path("bogus_ws");
    fake_package(&ws, "bogus", "<depend>made_up_pkg_xyz</depend>");
    sb.ruckus("/bin/bash")
        .args(["deps", "--distro", "jazzy", "--dry-run"])
        .arg(&ws)
        .assert()
        .failure()
        .stderr(predicate::str::contains("none of the dependencies"));
}

/// Values ruckus offers for the last word of `line` (not flags), asked as the fish script asks.
fn complete(sb: &Sandbox, line: &[&str]) -> Vec<String> {
    let out = sb
        .ruckus("/bin/bash")
        .env("COMPLETE", "fish")
        .arg("--")
        .args(line)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|line| line.split('\t').next().unwrap().to_string())
        .filter(|value| !value.starts_with('-'))
        .collect()
}

#[test]
fn completes_environment_and_distro_names() {
    let sb = Sandbox::new();
    assert!(complete(&sb, &["ruckus", "shell", ""]).is_empty());
    sb.create("lab");
    sb.create("cs422");
    // A broken file does not stop the others completing.
    std::fs::write(sb.path(".config/ruckus/envs/broken.toml"), "nope").unwrap();
    let distros = sb.path(".local/share/ruckus/distros");
    std::fs::create_dir_all(distros.join("jazzy")).unwrap();
    std::fs::write(distros.join("jazzy/ruckus.toml"), "").unwrap();
    // Not a finished install.
    std::fs::create_dir_all(distros.join("partial")).unwrap();

    let all = ["broken", "cs422", "lab"];
    assert_eq!(complete(&sb, &["ruckus", "shell", ""]), all);
    assert_eq!(complete(&sb, &["ruckus", "run", "-e", ""]), all);
    assert_eq!(complete(&sb, &["ruckus", "run", "--env", "l"]), ["lab"]);
    assert_eq!(
        complete(&sb, &["ruckus", "up", "turtle", "-e", "c"]),
        ["cs422"]
    );
    assert_eq!(complete(&sb, &["ruckus", "env", "show", ""]), all);
    assert_eq!(complete(&sb, &["ruckus", "env", "rm", "b"]), ["broken"]);
    assert_eq!(complete(&sb, &["ruckus", "uninstall", ""]), ["jazzy"]);
}

#[test]
fn completion_scripts_work_in_real_shells() {
    let sb = Sandbox::new();
    sb.create("lab");
    // The scripts call `ruckus` on PATH.
    let bin_dir = Path::new(env!("CARGO_BIN_EXE_ruckus")).parent().unwrap();
    let path = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let script = |shell: &str| {
        let out = sb
            .ruckus("/bin/bash")
            .args(["completions", shell])
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        let file = sb.path(shell).join("_ruckus");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, out.stdout).unwrap();
        file
    };
    let run = |shell: &str, code: &str, arg: &Path| {
        let out = sb
            .command(shell, shell)
            .env("PATH", &path)
            .args(["-c", code, "test"])
            .arg(arg)
            .output()
            .unwrap();
        assert!(out.status.success(), "{shell}: {out:?}");
        String::from_utf8(out.stdout).unwrap()
    };

    for shell in shells() {
        if shell.ends_with("/bash") {
            let file = script("bash");
            let out = run(
                &shell,
                r#"source "$1"
                COMP_WORDS=(ruckus shell l)
                COMP_CWORD=2
                _clap_complete_ruckus ruckus l shell
                printf '%s\n' "${COMPREPLY[@]}""#,
                &file,
            );
            assert_eq!(out, "lab\n", "bash");
        } else {
            let file = script("zsh");
            // Stand-ins for the completion system, which only loads in an interactive shell.
            let stubs = r#"compdef() { :; }
                _describe() { shift 2; print -rl -- "${(@P)1}"; }
                words=(ruckus shell l)
                CURRENT=3
                "#;
            // Sourced, as with `source <(ruckus completions zsh)`.
            let out = run(
                &shell,
                &format!("{stubs}source \"$1\"; _clap_dynamic_completer_ruckus"),
                &file,
            );
            assert_eq!(out, "lab\n", "zsh, sourced");
            // From $fpath, as `_ruckus`: it must complete on the first Tab too.
            let out = run(
                &shell,
                &format!("{stubs}fpath=(\"${{1:h}}\" $fpath); autoload -Uz _ruckus; _ruckus"),
                &file,
            );
            assert_eq!(out, "lab\n", "zsh, from fpath");
        }
    }
}
