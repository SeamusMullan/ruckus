# ruckus

Start a whole ROS 2 setup with one command.

ruckus opens a tmux session, splits it into panes, loads ROS 2 and your
workspace in each one, and starts a command in each: nodes, teleop, topic
echoes, RViz. Every pane stays a normal shell, so you can Ctrl+C a node and
press Up, Enter to run it again.

```console
$ ruckus up turtle
```

```
┌──────────────────────────┬──────────────────────────┐
│ turtlesim_node           │ turtle_teleop_key  ◀ keys│
├──────────────────────────┼──────────────────────────┤
│ topic echo /cmd_vel      │ topic list -v            │
└──────────────────────────┴──────────────────────────┘
```

It can also install ROS 2 for you, ready-built, on macOS and Linux:

```console
$ ruckus install jazzy
ros-jazzy-desktop 0.11.0 with Python 3.12.15: 702 packages, 871 MB to download (less if cached)
installed ROS 2 jazzy in ~/.local/share/ruckus/distros/jazzy
created environment `jazzy`
$ ruckus up turtle -e jazzy
```

> **Status:** early. Layouts, environments and installs work today. Source
> builds for distros with no ready-built packages are next; see the
> [roadmap](#roadmap).

## Install

Needs tmux (`brew install tmux` or `apt install tmux`).

```sh
cargo install --git https://github.com/SeamusMullan/ruckus
```

## Use

```sh
ruckus ls                    # list layouts
ruckus show robot            # what a layout runs
ruckus up turtle             # start it (or attach if running)
ruckus up robot -w ~/my_ws   # use a different workspace
ruckus up turtle --dry-run   # print the tmux commands instead
ruckus up turtle --restart   # stop and start again
ruckus down turtle           # stop it
```

Inside the session: click a pane (or Ctrl+B, arrow) to move, Ctrl+B z to zoom
a pane, Ctrl+B d to leave it running.

## Install ROS 2

```sh
ruckus install jazzy                 # desktop: RViz, turtlesim, demos, plus colcon and compilers
ruckus install kilted --variant base # no GUI tools
ruckus install jazzy --add ros-jazzy-navigation2   # extra packages; rerun to add more or update
ruckus install jazzy --dry-run       # what it would install, and the download size
ruckus distros                       # what is installed
ruckus uninstall jazzy
```

Packages come from [RoboStack](https://robostack.github.io), which builds ROS
for conda on macOS (Apple Silicon and Intel), Linux and Windows. ruckus
resolves and installs them itself through
[rattler](https://github.com/conda/rattler), the library behind pixi, so
neither conda nor pixi is needed. Each distro is self-contained in
`~/.local/share/ruckus/distros/<distro>`, with its own Python, and downloads
are shared through the rattler cache (the same one pixi uses).

`install` also creates an environment named after the distro, so
`ruckus shell jazzy` works straight away. To use an installed distro in another
environment, pass its name: `ruckus env create lab --ros jazzy --ws ~/lab_ws`.

`ros2_jazzy` works as a name too. Installing a distro RoboStack does not build
for your platform (such as Lyrical on macOS) says so; point an environment at
your own source build instead.

## Install a workspace's dependencies

```sh
ruckus deps -e jazzy                 # the workspace in the current directory
ruckus deps ~/nav_ws --distro jazzy  # or name it, and the distro
ruckus deps -e jazzy --dry-run       # what is installed and what is missing
```

`deps` does what `rosdep install` does, for a distro from `ruckus install`.
It reads every `package.xml` in the workspace (skipping `COLCON_IGNORE`d
ones), leaves out packages the workspace builds itself, and installs the rest
into the distro. ROS packages become `ros-jazzy-<name>`; system dependencies
such as `python3-numpy` or `eigen` use RoboStack's rosdep mapping, which ruckus
carries a copy of. Anything RoboStack does not have is listed, not installed.
It does not work with a ROS built from source yet; use rosdep there.

## Environments

An environment is a ROS install plus the workspaces you overlay on it, saved
under a name. Use one to get a shell, run a single command, or start a layout.

```sh
ruckus env create lab --ros ~/ros2_lyrical/install --ws ~/polar_ws --var ROS_DOMAIN_ID=7
ruckus env ls                     # * marks the default
ruckus env show lab               # checks the install and workspaces exist and are built
ruckus shell lab                  # your usual shell, with ROS and ~/polar_ws loaded
ruckus run -e lab -- ros2 topic list
ruckus up robot -e lab            # a layout in that environment
ruckus env rm lab                 # forgets it; never touches the install
```

`--ros` takes an install directory (ruckus picks `setup.zsh` or
`setup.bash` to match your shell) or a setup file. `--ws` can repeat; the
workspaces are overlaid in order, and layouts without their own workspace run
in the last one. `--pre` adds shell lines that run before ROS loads, such as a
`PATH` change.

Each environment is a small TOML file in `~/.config/ruckus/envs/` that you can
edit by hand. Set a default with `env = "lab"` under `[defaults]` in
`config.toml`; then `ruckus shell`, `ruckus run` and `ruckus up` use it
without `-e`.

`ruckus shell` runs your normal rc files first (`.zshrc` or `.bashrc`), so your
prompt and aliases stay, and sets `RUCKUS_ENV` to the environment's name.
If your rc file already loads a ROS, ruckus unloads it before loading the
environment's, so two distros never mix. Settings such as `ROS_DOMAIN_ID`
stay. Distros from `ruckus install` also get their own colcon settings
(`COLCON_HOME`), so `~/.colcon` tuned for another ROS does not leak in.
ROS is always loaded by sourcing its setup files inside the shell. On macOS
this matters: System Integrity Protection strips the `DYLD_*` variables ROS
needs from any environment passed to `/bin/zsh` or `/bin/bash`.

## Configure

ruckus reads `~/.config/ruckus/config.toml` (or `$XDG_CONFIG_HOME/ruckus/`,
or `--config`). Layouts there add to the built-in ones, and replace them when
the name matches.

```toml
[defaults]
env = "lab"                               # environment used when none is named
ros_setup = "/opt/ros/jazzy"              # or a ROS install, without an environment
pre = []                                  # shell lines run before ROS loads

[layouts.nav]
description = "Nav2 with RViz"
workspace = "~/nav_ws"                    # overlaid and cd'd into
requires = ["my_robot_bringup"]           # must be built in <workspace>/install
hint = "Run colcon build in ~/nav_ws first."
panes = [
  { run = "ros2 launch my_robot_bringup robot.launch.py" },
  { run = "ros2 launch nav2_bringup navigation_launch.py", delay = 5 },
  { run = "rviz2", delay = 5 },
  { focus = true },                       # an empty pane is a ready shell
]
```

| Pane key | Meaning |
|----------|---------|
| `run`    | Command to start. Empty leaves a shell. |
| `delay`  | Seconds to wait first, so earlier nodes are up. |
| `focus`  | This pane gets the keyboard. |

A layout can also set `env`, `ros_setup` and `pre`. The command line beats the
layout, the layout beats the environment, and the environment beats
`[defaults]`. `pre` lines add up instead: defaults, then environment, then
layout.

Built-in layouts: `turtle` (turtlesim), `pubsub` (the Python talker/listener
tutorial) and `robot` (a URDF package with RViz). See
[`src/builtin.toml`](src/builtin.toml).

## Roadmap

1. **`ruckus up`**: tmux layouts from config. *Done.*
2. **`ruckus env`**: named environments, each a ROS install plus workspaces,
   used by `up`, `shell` and `run`. *Done.*
3. **`ruckus install <distro>`**: ready-built ROS 2 from RoboStack, installed
   natively through the rattler library, no pixi or conda needed. *Done.*
4. **Source builds**: an automated, repeatable source build for distros with
   no ready-built packages on your platform (e.g. Lyrical on macOS).

## Develop

```sh
cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test
```

The tmux tests run on a private server (`RUCKUS_TMUX_SOCKET`), so they never
touch your own sessions. Tests that need the network (resolving real RoboStack
packages) run when `RUCKUS_NETWORK_TESTS=1` is set.

CI runs format, lint, tests and a smoke test of the release build on Linux and
macOS, plus a real `ruckus install jazzy` that passes a message between two
nodes and builds a package with colcon. It also runs weekly, to notice when
upstream packages change.

## License

Apache License 2.0. See [LICENSE](LICENSE).
