//! Open a tmux session in a new OS window of the user's terminal, so the
//! terminal they ran `ruckus up` from stays free (for `ruckus down`, say).
//!
//! Terminals are recognised by the environment variables they set. When there
//! is no window to open (over SSH, on Linux without a display) or the terminal
//! is unknown, the caller attaches in place instead.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::tmux::quote;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalApp {
    /// `app` is the kitty.app bundle on macOS.
    Kitty {
        app: Option<PathBuf>,
    },
    AppleTerminal,
    ITerm,
    /// `app` is the Ghostty.app bundle on macOS.
    Ghostty {
        app: Option<PathBuf>,
    },
    WezTerm {
        exe: Option<PathBuf>,
    },
    Alacritty,
    GnomeTerminal,
    Konsole,
}

impl TerminalApp {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Kitty { .. } => "kitty",
            Self::AppleTerminal => "Terminal",
            Self::ITerm => "iTerm2",
            Self::Ghostty { .. } => "Ghostty",
            Self::WezTerm { .. } => "WezTerm",
            Self::Alacritty => "Alacritty",
            Self::GnomeTerminal => "GNOME Terminal",
            Self::Konsole => "Konsole",
        }
    }
}

/// Why a new window cannot be opened, for the note shown when attaching in place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoWindow {
    Ssh,
    NoDisplay,
    Unknown,
}

impl NoWindow {
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Ssh => "this is an SSH session",
            Self::NoDisplay => "there is no display",
            Self::Unknown => "ruckus does not know how to open a window in this terminal",
        }
    }
}

/// The terminal to open a window in, from the environment, on `os`
/// (`std::env::consts::OS`).
pub fn detect(env: &HashMap<String, String>, os: &str) -> Result<TerminalApp, NoWindow> {
    let get = |name: &str| env.get(name).map(String::as_str).unwrap_or_default();
    let set = |name: &str| !get(name).is_empty();
    if set("SSH_CONNECTION") || set("SSH_TTY") {
        return Err(NoWindow::Ssh);
    }
    if os != "macos" && !set("DISPLAY") && !set("WAYLAND_DISPLAY") {
        return Err(NoWindow::NoDisplay);
    }
    // The app bundle holding a macOS terminal's resources dir, e.g.
    // /Applications/kitty.app/Contents/Resources/kitty -> /Applications/kitty.app
    let bundle = |dir: &str| {
        Path::new(dir)
            .ancestors()
            .find(|p| p.extension().is_some_and(|ext| ext == "app"))
            .map(Path::to_path_buf)
    };
    let program = get("TERM_PROGRAM");
    if set("KITTY_WINDOW_ID") || get("TERM") == "xterm-kitty" {
        return Ok(TerminalApp::Kitty {
            app: bundle(get("KITTY_INSTALLATION_DIR")),
        });
    }
    if program == "ghostty" || set("GHOSTTY_RESOURCES_DIR") {
        return Ok(TerminalApp::Ghostty {
            app: bundle(get("GHOSTTY_RESOURCES_DIR")),
        });
    }
    if program == "WezTerm" {
        return Ok(TerminalApp::WezTerm {
            exe: env.get("WEZTERM_EXECUTABLE").map(PathBuf::from),
        });
    }
    if program == "iTerm.app" {
        return Ok(TerminalApp::ITerm);
    }
    if program == "Apple_Terminal" {
        return Ok(TerminalApp::AppleTerminal);
    }
    if set("ALACRITTY_WINDOW_ID") || set("ALACRITTY_SOCKET") {
        return Ok(TerminalApp::Alacritty);
    }
    if set("KONSOLE_VERSION") {
        return Ok(TerminalApp::Konsole);
    }
    if set("GNOME_TERMINAL_SCREEN") || set("GNOME_TERMINAL_SERVICE") {
        return Ok(TerminalApp::GnomeTerminal);
    }
    Err(NoWindow::Unknown)
}

/// How to open the window: the program and its arguments, and whether that
/// program returns at once (so its exit status can be checked) or stays
/// running as the window itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub argv: Vec<String>,
    pub returns: bool,
}

/// The command that opens a new window of `terminal` running `attach`
/// (a full tmux command line with absolute paths, since some launchers start
/// the window with a minimal environment).
pub fn launch(terminal: &TerminalApp, attach: &[String], os: &str) -> Launch {
    let mut argv: Vec<String> = vec![];
    let mut returns = true;
    let line = attach
        .iter()
        .map(|arg| quote(arg))
        .collect::<Vec<_>>()
        .join(" ");
    let push = |argv: &mut Vec<String>, items: &[&str]| {
        argv.extend(items.iter().map(|s| s.to_string()));
    };
    match terminal {
        TerminalApp::Kitty { app } | TerminalApp::Ghostty { app } if os == "macos" => {
            // A second instance of the app is a new OS window; `open -n` starts one.
            let default = if matches!(terminal, TerminalApp::Kitty { .. }) {
                "kitty"
            } else {
                "Ghostty"
            };
            push(&mut argv, &["open", "-n", "-a"]);
            argv.push(
                app.as_ref()
                    .map_or(default.to_string(), |p| p.display().to_string()),
            );
            push(&mut argv, &["--args"]);
            if matches!(terminal, TerminalApp::Ghostty { .. }) {
                // Like kitty below: end this instance with its window.
                push(&mut argv, &["--quit-after-last-window-closed=true", "-e"]);
            } else {
                // A macOS app stays running with no windows by default, which
                // would leave an idle kitty behind after `ruckus down`.
                push(&mut argv, &["-o", "macos_quit_when_last_window_closed=yes"]);
            }
            argv.extend(attach.iter().cloned());
        }
        TerminalApp::Kitty { .. } => {
            push(&mut argv, &["kitty", "--detach"]);
            argv.extend(attach.iter().cloned());
        }
        TerminalApp::Ghostty { .. } => {
            push(&mut argv, &["ghostty", "-e"]);
            argv.extend(attach.iter().cloned());
            returns = false;
        }
        TerminalApp::AppleTerminal => {
            push(&mut argv, &["osascript", "-e"]);
            argv.push(format!(
                "tell application \"Terminal\"\n  activate\n  do script \"{}\"\nend tell",
                applescript_escape(&line)
            ));
        }
        TerminalApp::ITerm => {
            push(&mut argv, &["osascript", "-e"]);
            argv.push(format!(
                "tell application \"iTerm\"\n  activate\n  create window with default profile command \"{}\"\nend tell",
                applescript_escape(&line)
            ));
        }
        TerminalApp::WezTerm { exe } => {
            argv.push(
                exe.as_ref()
                    .map_or("wezterm".to_string(), |p| p.display().to_string()),
            );
            push(&mut argv, &["start", "--always-new-process", "--"]);
            argv.extend(attach.iter().cloned());
            returns = false;
        }
        TerminalApp::Alacritty => {
            push(&mut argv, &["alacritty", "-e"]);
            argv.extend(attach.iter().cloned());
            returns = false;
        }
        TerminalApp::GnomeTerminal => {
            push(&mut argv, &["gnome-terminal", "--window", "--"]);
            argv.extend(attach.iter().cloned());
        }
        TerminalApp::Konsole => {
            push(&mut argv, &["konsole", "-e"]);
            argv.extend(attach.iter().cloned());
            returns = false;
        }
    }
    Launch { argv, returns }
}

fn applescript_escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    const ATTACH: [&str; 6] = [
        "/opt/homebrew/bin/tmux",
        "-S",
        "/tmp/tmux-501/default",
        "attach-session",
        "-t",
        "=ruckus-turtle",
    ];

    fn attach() -> Vec<String> {
        ATTACH.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn detects_kitty_and_its_app_bundle() {
        let term = detect(
            &env(&[
                ("TERM", "xterm-kitty"),
                ("KITTY_WINDOW_ID", "50"),
                (
                    "KITTY_INSTALLATION_DIR",
                    "/Applications/kitty.app/Contents/Resources/kitty",
                ),
            ]),
            "macos",
        );
        assert_eq!(
            term,
            Ok(TerminalApp::Kitty {
                app: Some("/Applications/kitty.app".into())
            })
        );
    }

    #[test]
    fn detects_other_terminals() {
        let cases = [
            (
                vec![("TERM_PROGRAM", "Apple_Terminal")],
                TerminalApp::AppleTerminal,
            ),
            (vec![("TERM_PROGRAM", "iTerm.app")], TerminalApp::ITerm),
            (
                vec![
                    ("TERM_PROGRAM", "WezTerm"),
                    ("WEZTERM_EXECUTABLE", "/x/wezterm"),
                ],
                TerminalApp::WezTerm {
                    exe: Some("/x/wezterm".into()),
                },
            ),
            (vec![("ALACRITTY_WINDOW_ID", "1")], TerminalApp::Alacritty),
            (
                vec![("TERM_PROGRAM", "ghostty")],
                TerminalApp::Ghostty { app: None },
            ),
        ];
        for (vars, expected) in cases {
            assert_eq!(detect(&env(&vars), "macos"), Ok(expected));
        }
        assert_eq!(
            detect(
                &env(&[("GNOME_TERMINAL_SCREEN", "/org/x"), ("DISPLAY", ":0")]),
                "linux"
            ),
            Ok(TerminalApp::GnomeTerminal)
        );
    }

    #[test]
    fn no_window_over_ssh_without_a_display_or_in_an_unknown_terminal() {
        assert_eq!(
            detect(
                &env(&[
                    ("KITTY_WINDOW_ID", "1"),
                    ("SSH_CONNECTION", "1.2.3.4 5 6.7.8.9 22")
                ]),
                "macos"
            ),
            Err(NoWindow::Ssh)
        );
        assert_eq!(
            detect(&env(&[("KITTY_WINDOW_ID", "1")]), "linux"),
            Err(NoWindow::NoDisplay)
        );
        assert_eq!(
            detect(&env(&[("TERM", "xterm-256color")]), "macos"),
            Err(NoWindow::Unknown)
        );
    }

    #[test]
    fn kitty_opens_a_new_instance() {
        let app = TerminalApp::Kitty {
            app: Some("/Applications/kitty.app".into()),
        };
        let mac = launch(&app, &attach(), "macos");
        assert_eq!(
            mac.argv[..5],
            ["open", "-n", "-a", "/Applications/kitty.app", "--args"]
        );
        assert_eq!(
            mac.argv[5..7],
            ["-o", "macos_quit_when_last_window_closed=yes"]
        );
        assert_eq!(mac.argv[7..], ATTACH);
        assert!(mac.returns);

        let linux = launch(&TerminalApp::Kitty { app: None }, &attach(), "linux");
        assert_eq!(linux.argv[..2], ["kitty", "--detach"]);
        assert_eq!(linux.argv[2..], ATTACH);
    }

    #[test]
    fn applescript_terminals_get_one_quoted_command_line() {
        let mut attach = attach();
        attach[2] = "/tmp/my \"odd\" dir/default".into();
        let launch = launch(&TerminalApp::AppleTerminal, &attach, "macos");
        assert_eq!(launch.argv[0], "osascript");
        assert!(
            launch.argv[2].contains(
                "do script \"/opt/homebrew/bin/tmux -S '/tmp/my \\\"odd\\\" dir/default' attach-session -t =ruckus-turtle\""
            ),
            "{}",
            launch.argv[2]
        );
    }
}
