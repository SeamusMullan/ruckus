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
