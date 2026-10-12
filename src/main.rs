mod config;
mod tmux;

use std::os::unix::process::CommandExt;
use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::{Args, Parser, Subcommand};

use config::Config;
use tmux::{Overrides, Plan, RealTmux, Recorder, Tmux, quote};

/// Start ROS 2 sessions in tmux with one command.
#[derive(Parser)]
#[command(name = "ruckus", version, after_help = AFTER_HELP)]
struct Cli {
    /// Config file [default: ~/.config/ruckus/config.toml]
    #[arg(long, global = true, env = "RUCKUS_CONFIG")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

const AFTER_HELP: &str = "\
Examples:
  ruckus ls                     list layouts
  ruckus up turtle              start turtlesim, teleop and echoes in tmux
  ruckus up robot --dry-run     print the tmux commands instead of running them
  ruckus down turtle            stop the session";

#[derive(Subcommand)]
enum Command {
    /// Start a layout in tmux, or attach to it if it is already running
    Up(UpArgs),
    /// Stop a layout's tmux session
    Down {
        layout: String,
        /// Session name [default: ruckus-<layout>]
        #[arg(long)]
        session: Option<String>,
    },
    /// List layouts
    #[command(alias = "layouts")]
    Ls,
    /// Show what a layout runs
    Show { layout: String },
}

#[derive(Args)]
struct UpArgs {
    layout: String,
    /// Session name [default: ruckus-<layout>]
    #[arg(long)]
    session: Option<String>,
    /// Workspace to overlay, instead of the layout's
    #[arg(long, short)]
    workspace: Option<PathBuf>,
    /// ROS setup file to source, instead of the configured one
    #[arg(long)]
    ros_setup: Option<PathBuf>,
    /// Start the session without attaching to it
    #[arg(long, short)]
    detach: bool,
    /// Stop a running session of the same name first
    #[arg(long)]
    restart: bool,
    /// Print the tmux commands instead of running them
    #[arg(long, short = 'n')]
    dry_run: bool,
}

fn main() {
    if let Err(err) = run(Cli::parse()) {
        eprintln!("error: {err:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    let config = Config::load(cli.config.as_deref())?;
    match cli.command {
        Command::Up(args) => up(&config, args),
        Command::Down { layout, session } => {
            let session = session.unwrap_or_else(|| session_name(&layout));
            let mut tmux = RealTmux::from_env();
            if !tmux.has_session(&session) {
                bail!("no session named {session}");
            }
            tmux.run(&["kill-session".into(), "-t".into(), format!("={session}")])?;
            println!("stopped {session}");
            Ok(())
        }
        Command::Ls => {
            let width = config.layouts.keys().map(String::len).max().unwrap_or(0);
            for (name, layout) in &config.layouts {
                println!("{name:width$}  {}", layout.description);
            }
            Ok(())
        }
        Command::Show { layout: name } => {
            let layout = config.layout(&name)?;
            println!("{name}: {}", layout.description);
            if let Some(workspace) = &layout.workspace {
                println!("workspace: {workspace}");
            }
            if !layout.requires.is_empty() {
                println!("requires:  {}", layout.requires.join(", "));
            }
            for (i, pane) in layout.panes.iter().enumerate() {
                let mut notes = vec![];
                if pane.delay > 0 {
                    notes.push(format!("after {}s", pane.delay));
                }
                if pane.focus {
                    notes.push("focus".into());
                }
                let notes = if notes.is_empty() {
                    String::new()
                } else {
                    format!("  ({})", notes.join(", "))
                };
                let run = if pane.run.is_empty() {
                    "(shell)"
                } else {
                    &pane.run
                };
                println!("  {}  {run}{notes}", i + 1);
            }
            Ok(())
        }
    }
}

fn up(config: &Config, args: UpArgs) -> Result<()> {
    let layout = config.layout(&args.layout)?;
    let session = args.session.unwrap_or_else(|| session_name(&args.layout));
    let overrides = Overrides {
        workspace: args.workspace,
        ros_setup: args.ros_setup,
    };
    let plan = Plan::new(&args.layout, &session, layout, &config.defaults, &overrides)?;

    if args.dry_run {
        let mut recorder = Recorder::default();
        plan.execute(&mut recorder)?;
        print!("{}", recorder.script());
        return Ok(());
    }

    let mut tmux = RealTmux::from_env();
    if tmux.has_session(&session) {
        if args.restart {
            tmux.run(&["kill-session".into(), "-t".into(), format!("={session}")])?;
        } else {
            eprintln!("{session} is already running; attaching (use --restart to start it again)");
        }
    }
    if !tmux.has_session(&session) {
        plan.execute(&mut tmux)?;
    }

    if args.detach {
        println!(
            "started {session}; attach with: tmux attach -t {}",
            quote(&session)
        );
        return Ok(());
    }
    let mut attach = tmux.command();
    if std::env::var_os("TMUX").is_some() {
        attach.args(["switch-client", "-t", &format!("={session}")]);
    } else {
        attach.args(["attach-session", "-t", &format!("={session}")]);
    }
    Err(attach.exec().into())
}

fn session_name(layout: &str) -> String {
    format!("ruckus-{layout}")
}
