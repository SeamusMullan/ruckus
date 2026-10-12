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
        .stdout(predicate::str::contains("urdf"));
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
        .args(["up", "turtle", "--dry-run", "--here"])
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
        .args(["up", "pubsub", "--dry-run", "--workspace"])
        .arg(ws.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("not built in"))
        .stderr(predicate::str::contains("py_pubsub"))
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

/// Values ruckus offers for the last word of `line` (not flags), asked as the fish script asks.
fn complete(config_home: &Path, line: &[&str]) -> Vec<String> {
    let out = ruckus(config_home)
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
fn prints_completion_scripts() {
    let home = tempfile::tempdir().unwrap();
    // A broken config must not break completion setup in the user's rc file.
    std::fs::create_dir_all(home.path().join("ruckus")).unwrap();
    std::fs::write(home.path().join("ruckus/config.toml"), "nope").unwrap();
    for (shell, expected) in [
        ("zsh", "#compdef ruckus"),
        ("bash", "-F _clap_complete_ruckus ruckus"),
        ("fish", "complete --keep-order --exclusive --command ruckus"),
    ] {
        ruckus(home.path())
            .args(["completions", shell])
            .assert()
            .success()
            .stdout(predicate::str::contains(expected))
            .stdout(predicate::str::contains("COMPLETE="));
    }
    ruckus(home.path())
        .args(["completions", "tcsh"])
        .assert()
        .failure();
}

#[test]
fn completes_subcommands_and_layouts() {
    let home = tempfile::tempdir().unwrap();
    let commands = complete(home.path(), &["ruckus", ""]);
    for command in ["up", "down", "env", "shell", "install", "completions"] {
        assert!(
            commands.iter().any(|c| c == command),
            "{command}: {commands:?}"
        );
    }
    assert!(!commands.iter().any(|c| c == "internal"), "{commands:?}");

    std::fs::create_dir_all(home.path().join("ruckus")).unwrap();
    std::fs::write(
        home.path().join("ruckus/config.toml"),
        "[layouts.mine]\npanes = [{ run = \"echo\" }]\n",
    )
    .unwrap();
    assert_eq!(
        complete(home.path(), &["ruckus", "up", ""]),
        ["mine", "pubsub", "turtle", "urdf"]
    );
    assert_eq!(complete(home.path(), &["ruckus", "show", "tu"]), ["turtle"]);
    assert_eq!(complete(home.path(), &["ruckus", "down", "p"]), ["pubsub"]);
    assert_eq!(
        complete(home.path(), &["ruckus", "completions", ""]),
        ["zsh", "bash", "fish"]
    );
}

/// `ruckus` with no hint of which terminal it runs in.
fn ruckus_in_terminal(home: &Path, vars: &[(&str, &str)]) -> assert_cmd::Command {
    let mut cmd = ruckus(home);
    for name in [
        "KITTY_WINDOW_ID",
        "KITTY_INSTALLATION_DIR",
        "TERM_PROGRAM",
        "TERM",
        "GHOSTTY_RESOURCES_DIR",
        "ALACRITTY_WINDOW_ID",
        "ALACRITTY_SOCKET",
        "KONSOLE_VERSION",
        "GNOME_TERMINAL_SCREEN",
        "GNOME_TERMINAL_SERVICE",
        "WEZTERM_EXECUTABLE",
        "SSH_CONNECTION",
        "SSH_TTY",
    ] {
        cmd.env_remove(name);
    }
    cmd.env("DISPLAY", ":0");
    for (name, value) in vars {
        cmd.env(name, value);
    }
    cmd
}

#[test]
fn up_opens_a_new_terminal_window() {
    let home = tempfile::tempdir().unwrap();
    let kitty = [
        ("KITTY_WINDOW_ID", "1"),
        (
            "KITTY_INSTALLATION_DIR",
            "/Applications/kitty.app/Contents/Resources/kitty",
        ),
    ];
    let expected = if cfg!(target_os = "macos") {
        "# then, in a new kitty window: open -n -a /Applications/kitty.app --args -o macos_quit_when_last_window_closed=yes tmux attach-session -t =ruckus-turtle\n"
    } else {
        "# then, in a new kitty window: kitty --detach tmux attach-session -t =ruckus-turtle\n"
    };
    ruckus_in_terminal(home.path(), &kitty)
        .args(["up", "turtle", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::ends_with(expected));

    // --here, --detach or `window = false` keep the old behaviour.
    for extra in ["--here", "--detach"] {
        ruckus_in_terminal(home.path(), &kitty)
            .args(["up", "turtle", "--dry-run", extra])
            .assert()
            .success()
            .stdout(predicate::str::contains("# then").not());
    }
    std::fs::create_dir(home.path().join("ruckus")).unwrap();
    std::fs::write(
        home.path().join("ruckus/config.toml"),
        "[defaults]\nwindow = false\n",
    )
    .unwrap();
    ruckus_in_terminal(home.path(), &kitty)
        .args(["up", "turtle", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("# then").not());
}

#[test]
fn up_attaches_here_when_no_window_can_open() {
    let home = tempfile::tempdir().unwrap();
    ruckus_in_terminal(
        home.path(),
        &[
            ("KITTY_WINDOW_ID", "1"),
            ("SSH_CONNECTION", "1.2.3.4 5 6.7.8.9 22"),
        ],
    )
    .args(["up", "turtle", "--dry-run"])
    .assert()
    .success()
    .stdout(predicate::str::ends_with(
        "# then attach here (this is an SSH session)\n",
    ));
    ruckus_in_terminal(home.path(), &[("TERM", "xterm-256color")])
        .args(["up", "turtle", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "ruckus does not know how to open a window in this terminal",
        ));
}
