mod config;
mod env;
mod shell;
mod tmux;

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use clap::{Args, Parser, Subcommand};

use config::{Config, expand_tilde};
use env::Env;
use shell::{Activation, Shell};
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
  ruckus down turtle            stop the session

  ruckus env create cs422 --ros ~/ros2_lyrical/install --ws ~/polar_ws
  ruckus shell cs422            a shell with that ROS and workspace loaded
  ruckus run -e cs422 -- ros2 topic list
  ruckus up robot -e cs422      a layout in that environment";

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
    /// Manage environments: a ROS install plus workspaces and settings
    #[command(subcommand)]
    Env(EnvCommand),
    /// Open a shell with an environment loaded
    Shell {
        /// Environment [default: `env` in [defaults]]
        env: Option<String>,
    },
    /// Run one command with an environment loaded
    Run {
        /// Environment [default: `env` in [defaults]]
        #[arg(long, short)]
        env: Option<String>,
        /// The command and its arguments
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
}

#[derive(Subcommand)]
enum EnvCommand {
    /// Create an environment
    Create {
        name: String,
        /// ROS install directory, or its setup file
        #[arg(long)]
        ros: PathBuf,
        /// Workspace to overlay; repeat for more, in order
        #[arg(long = "ws", value_name = "DIR")]
        workspaces: Vec<PathBuf>,
        /// Variable to export, as NAME=VALUE; repeat for more
        #[arg(long = "var", value_name = "NAME=VALUE", value_parser = parse_var)]
        vars: Vec<(String, String)>,
        /// Shell line to run before ROS loads, e.g. a PATH change; repeat for more
        #[arg(long = "pre", value_name = "LINE", allow_hyphen_values = true)]
        pre: Vec<String>,
        /// Replace an environment with the same name
        #[arg(long)]
        force: bool,
    },
    /// List environments
    Ls,
    /// Show an environment and check that its parts exist
    Show { name: String },
    /// Delete an environment (the ROS install and workspaces are not touched)
    Rm { name: String },
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
    /// Environment to load, instead of the layout's or the default
    #[arg(long, short)]
    env: Option<String>,
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
        Command::Env(command) => env_command(&config, command),
        Command::Shell { env } => {
            let (name, activation) = activation(&config, env)?;
            if let Ok(current) = std::env::var("RUCKUS_ENV") {
                eprintln!("ruckus: note: already inside {current}; this shell nests inside it");
            }
            shell::exec_interactive(&name, &activation, Shell::from_env())
        }
        Command::Run { env, command } => {
            let (_, activation) = activation(&config, env)?;
            shell::exec_command(&activation, Shell::from_env(), &command)
        }
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
    let env_name = args
        .env
        .or_else(|| layout.env.clone())
        .or_else(|| config.defaults.env.clone());
    let env = env_name.as_deref().map(Env::load).transpose()?;
    let plan = Plan::new(
        &args.layout,
        &session,
        layout,
        &config.defaults,
        env.as_ref(),
        &overrides,
        Shell::from_env(),
    )?;

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

/// The named (or default) environment, resolved for the user's shell.
fn activation(config: &Config, name: Option<String>) -> Result<(String, Activation)> {
    let Some(name) = name.or_else(|| config.defaults.env.clone()) else {
        bail!("name an environment, or set `env` in [defaults] (see `ruckus env ls`)");
    };
    let env = Env::load(&name)?;
    let ros = env
        .ros_path()
        .or_else(|| config.defaults.ros_setup.as_deref().map(expand_tilde));
    let mut pre = config.defaults.pre.clone();
    pre.extend(env.pre.iter().cloned());
    let activation = Activation::resolve(
        ros.as_deref(),
        &env.workspace_paths(),
        pre,
        env.vars.clone(),
        Shell::from_env(),
    )?;
    Ok((name, activation))
}

fn env_command(config: &Config, command: EnvCommand) -> Result<()> {
    match command {
        EnvCommand::Create {
            name,
            ros,
            workspaces,
            vars,
            pre,
            force,
        } => {
            let ros_abs = expand_tilde(&ros.to_string_lossy());
            if !ros_abs.is_file() && !has_setup_file(&ros_abs) {
                bail!(
                    "{} is not a ROS install: expected a setup file, or a directory with setup.zsh, setup.bash or setup.sh",
                    ros.display()
                );
            }
            for workspace in &workspaces {
                let path = expand_tilde(&workspace.to_string_lossy());
                if !path.is_dir() {
                    bail!("workspace {} does not exist", workspace.display());
                }
                if !path.join("install").is_dir() {
                    eprintln!(
                        "note: {} is not built yet; it is loaded once `colcon build` has run",
                        workspace.display()
                    );
                }
            }
            let env = Env {
                ros: Some(env::store_path(&ros)?),
                workspaces: workspaces
                    .iter()
                    .map(|ws| env::store_path(ws))
                    .collect::<Result<_>>()?,
                pre,
                vars: vars.into_iter().collect(),
            };
            let path = env.save(&name, force)?;
            println!("created {name} ({})", path.display());
            println!("use it with: ruckus shell {name}");
            Ok(())
        }
        EnvCommand::Ls => {
            let envs = env::list()?;
            if envs.is_empty() {
                println!("no environments yet; create one with `ruckus env create`");
                return Ok(());
            }
            let default = config.defaults.env.as_deref();
            let width = envs.iter().map(|(name, _)| name.len()).max().unwrap_or(0);
            for (name, env) in envs {
                let mark = if Some(name.as_str()) == default {
                    "*"
                } else {
                    " "
                };
                let mut parts = vec![env.ros.unwrap_or_else(|| "(no ROS)".into())];
                parts.extend(env.workspaces);
                println!("{mark} {name:width$}  {}", parts.join(" + "));
            }
            Ok(())
        }
        EnvCommand::Show { name } => {
            let env = Env::load(&name)?;
            let shell = Shell::from_env();
            println!("{name}");
            match env.ros_path() {
                Some(ros) => {
                    let status = match shell::setup_file(&ros, shell.ext()) {
                        Ok(file) => format!("ok, {}", file.display()),
                        Err(err) => format!("PROBLEM: {err}"),
                    };
                    println!(
                        "  ros:       {}  ({status})",
                        env.ros.as_deref().unwrap_or_default()
                    );
                }
                None => println!("  ros:       (none)"),
            }
            for (workspace, path) in env.workspaces.iter().zip(env.workspace_paths()) {
                let status = if !path.is_dir() {
                    "PROBLEM: missing"
                } else if path.join("install").is_dir() {
                    "built"
                } else {
                    "not built"
                };
                println!("  workspace: {workspace}  ({status})");
            }
            for line in &env.pre {
                println!("  pre:       {line}");
            }
            for (key, value) in &env.vars {
                println!("  var:       {key}={value}");
            }
            println!("  file:      {}", env::path(&name)?.display());
            Ok(())
        }
        EnvCommand::Rm { name } => {
            env::remove(&name)?;
            println!("removed {name}");
            Ok(())
        }
    }
}

fn has_setup_file(dir: &Path) -> bool {
    ["zsh", "bash", "sh"]
        .iter()
        .any(|ext| dir.join(format!("setup.{ext}")).is_file())
}

fn parse_var(text: &str) -> Result<(String, String), String> {
    match text.split_once('=') {
        Some((name, value)) if !name.is_empty() => Ok((name.to_string(), value.to_string())),
        _ => Err(format!("expected NAME=VALUE, got `{text}`")),
    }
}
