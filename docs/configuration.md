# Configuration

ruckus reads `~/.config/ruckus/config.toml` (or `$XDG_CONFIG_HOME/ruckus/config.toml`,
or the file named by `--config` or `RUCKUS_CONFIG`). Everything in it is optional.

```toml
[defaults]
env = "lab"                      # environment used when none is named
ros_setup = "/opt/ros/jazzy"     # a ROS install to use without an environment
pre = []                         # shell lines run before ROS loads

[layouts.nav]
description = "Nav2 with RViz"
workspace = "~/nav_ws"           # overlaid on ROS, and each pane starts in it
requires = ["my_robot_bringup"]  # must be built in <workspace>/install
hint = "Run colcon build in ~/nav_ws first."
panes = [
  { run = "ros2 launch my_robot_bringup robot.launch.py" },
  { run = "ros2 launch nav2_bringup navigation_launch.py", delay = 5 },
  { run = "rviz2", delay = 5 },
  { focus = true },              # an empty pane is a ready shell
]
```

## Layouts

Layouts in your config add to the [built-in ones](../src/builtin.toml), and
replace a built-in one with the same name. Panes are tiled left to right, top
to bottom.

| Layout key    | Meaning |
|---------------|---------|
| `description` | Shown by `ruckus ls`. |
| `panes`       | The panes, in order. At least one. |
| `workspace`   | A colcon workspace to overlay on ROS. Panes start in it. |
| `requires`    | Packages that must be built in `<workspace>/install`, checked before the session starts. |
| `hint`        | Printed when a required package is missing. |
| `env`         | Environment to use, instead of `[defaults] env`. |
| `ros_setup`   | ROS install or setup file, instead of the environment's. |
| `pre`         | Shell lines run in every pane before ROS loads. |

| Pane key | Meaning |
|----------|---------|
| `run`    | Command to start. Leave it out for a plain shell. |
| `delay`  | Seconds to wait first, so earlier nodes are up. |
| `focus`  | This pane gets the keyboard. At most one per layout. |

## Which setting wins

For the ROS install and workspace: the command line (`--ros-setup`,
`--workspace`) beats the layout, the layout beats the environment, and the
environment beats `[defaults]`.

`pre` lines add up instead of replacing each other: those from `[defaults]`
run first, then the environment's, then the layout's.

## Environments

Each environment is a file in `~/.config/ruckus/envs/<name>.toml`, written by
`ruckus env create` and fine to edit by hand:

```toml
ros = "~/.local/share/ruckus/distros/jazzy"   # install dir, or a setup file
workspaces = ["~/lab_ws"]                     # overlaid in order
pre = ['export PATH="$HOME/bin:$PATH"']       # run before ROS loads
[vars]
ROS_DOMAIN_ID = "7"                           # exported after ROS loads
```

For an install directory, ruckus sources `setup.zsh` or `setup.bash` to match
your shell.

## Files ruckus writes

| Path | What |
|------|------|
| `~/.config/ruckus/envs/` | Environments. |
| `~/.local/share/ruckus/distros/<distro>/` | Installed distros: `prefix/` (the packages), `setup.*`, `colcon/` (its `COLCON_HOME`), `ruckus.toml` and `ruckus.lock`. |
| `~/.cache/ruckus/` | Shell wrappers for `ruckus shell`. |
| rattler's cache (`~/Library/Caches/rattler` or `~/.cache/rattler`) | Downloaded packages, shared with pixi. |

The `XDG_CONFIG_HOME`, `XDG_DATA_HOME` and `XDG_CACHE_HOME` variables move
these, as usual.
