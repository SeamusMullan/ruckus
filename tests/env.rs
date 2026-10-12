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
