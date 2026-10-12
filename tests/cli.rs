use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;

/// ruckus with an isolated config dir, so the user's own config never leaks in.
fn ruckus(config_home: &Path) -> assert_cmd::Command {
    let mut cmd = cargo_bin_cmd!("ruckus");
    cmd.env("XDG_CONFIG_HOME", config_home)
        .env_remove("RUCKUS_CONFIG")
        .env_remove("TMUX");
    cmd
}

#[test]
fn help_and_version() {
    let home = tempfile::tempdir().unwrap();
    ruckus(home.path())
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Start a layout in tmux"));
    ruckus(home.path())
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::starts_with("ruckus "));
}

#[test]
fn lists_builtin_layouts() {
    let home = tempfile::tempdir().unwrap();
    ruckus(home.path())
        .arg("ls")
        .assert()
        .success()
        .stdout(predicate::str::contains("turtle"))
        .stdout(predicate::str::contains("pubsub"))
        .stdout(predicate::str::contains("robot"));
}

#[test]
fn show_describes_panes() {
    let home = tempfile::tempdir().unwrap();
    ruckus(home.path())
        .args(["show", "turtle"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "2  ros2 run turtlesim turtle_teleop_key  (focus)",
        ))
        .stdout(predicate::str::contains("(after 3s)"));
}

#[test]
fn unknown_layout_fails() {
    let home = tempfile::tempdir().unwrap();
    ruckus(home.path())
        .args(["up", "nope", "--dry-run"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown layout `nope`"));
}

#[test]
fn dry_run_prints_tmux_commands() {
    let home = tempfile::tempdir().unwrap();
    ruckus(home.path())
        .args(["up", "turtle", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("tmux new-session -d"))
        .stdout(predicate::str::contains("-s ruckus-turtle"))
        .stdout(predicate::str::contains(
            "tmux send-keys -t %1 'ros2 run turtlesim turtle_teleop_key' Enter",
        ))
        .stdout(predicate::str::ends_with("tmux select-pane -t %1\n"));
}

#[test]
fn missing_workspace_package_fails_with_hint() {
    let home = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    ruckus(home.path())
        .args(["up", "robot", "--dry-run", "--workspace"])
        .arg(ws.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("not built in"))
        .stderr(predicate::str::contains("urdf_robot"))
        .stderr(predicate::str::contains("hint:"));
}

#[test]
fn reads_user_config_from_xdg_dir() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir(home.path().join("ruckus")).unwrap();
    std::fs::write(
        home.path().join("ruckus/config.toml"),
        "[layouts.mine]\ndescription = \"my layout\"\npanes = [{ run = \"echo hi\" }]\n",
    )
    .unwrap();
    ruckus(home.path())
        .arg("ls")
        .assert()
        .success()
        .stdout(predicate::str::contains("mine    my layout"));
}

#[test]
fn bad_config_is_reported() {
    let home = tempfile::tempdir().unwrap();
    let file = home.path().join("bad.toml");
    std::fs::write(&file, "[layouts.x]\npanes = []\n").unwrap();
    ruckus(home.path())
        .arg("--config")
        .arg(&file)
        .arg("ls")
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid layout `x`"));
}

/// Starts a real session on a private tmux server, checks the panes ran, then stops it.
#[test]
fn up_and_down_with_real_tmux() {
    if Command::new("tmux").arg("-V").output().is_err() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let socket = format!("ruckus-test-{}", std::process::id());
    // Stop the private server even when an assertion fails part way.
    struct KillServer<'a>(&'a str);
    impl Drop for KillServer<'_> {
        fn drop(&mut self) {
            let _ = Command::new("tmux")
                .args(["-L", self.0, "kill-server"])
                .output();
        }
    }
    let _server = KillServer(&socket);
    std::fs::create_dir(home.path().join("ruckus")).unwrap();
    std::fs::write(
        home.path().join("ruckus/config.toml"),
        r#"
        [layouts.demo]
        pre = ["export RUCKUS_MARK=ok"]
        panes = [
          { run = "echo pane-one-$RUCKUS_MARK" },
          { run = "echo pane-two-$RUCKUS_MARK", focus = true },
          { run = "echo pane-three-$RUCKUS_MARK", delay = 1 },
        ]
        "#,
    )
    .unwrap();
    let tmux = |args: &[&str]| {
        Command::new("tmux")
            .args(["-L", &socket])
            .args(args)
            .output()
            .unwrap()
    };

    ruckus(home.path())
        .env("RUCKUS_TMUX_SOCKET", &socket)
        .args(["up", "demo", "--detach"])
        .assert()
        .success()
        .stdout(predicate::str::contains("started ruckus-demo"));

    let panes = tmux(&[
        "list-panes",
        "-t",
        "=ruckus-demo",
        "-F",
        "#{pane_id} #{pane_active}",
    ]);
    let panes = String::from_utf8(panes.stdout).unwrap();
    let panes: Vec<&str> = panes.lines().collect();
    assert_eq!(panes.len(), 3, "{panes:?}");
    assert!(
        panes[1].ends_with(" 1"),
        "pane 2 should have focus: {panes:?}"
    );

    // Shells start at different speeds; wait for every pane to print its line.
    let deadline = Instant::now() + Duration::from_secs(20);
    for (pane, expected) in panes
        .iter()
        .zip(["pane-one-ok", "pane-two-ok", "pane-three-ok"])
    {
        let id = pane.split(' ').next().unwrap();
        loop {
            let out = tmux(&["capture-pane", "-p", "-t", id]);
            // Look for the output line, not the echoed command (which has `$RUCKUS_MARK`).
            if String::from_utf8_lossy(&out.stdout)
                .lines()
                .any(|line| line.trim() == expected)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "{expected} never appeared in {id}"
            );
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    // Running `up` again attaches instead of starting a second copy; `down` stops it.
    ruckus(home.path())
        .env("RUCKUS_TMUX_SOCKET", &socket)
        .args(["up", "demo", "--detach"])
        .assert()
        .success()
        .stderr(predicate::str::contains("already running"));
    ruckus(home.path())
        .env("RUCKUS_TMUX_SOCKET", &socket)
        .args(["down", "demo"])
        .assert()
        .success();
    assert!(
        !tmux(&["has-session", "-t", "=ruckus-demo"])
            .status
            .success()
    );
    ruckus(home.path())
        .env("RUCKUS_TMUX_SOCKET", &socket)
        .args(["down", "demo"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no session named ruckus-demo"));
}
