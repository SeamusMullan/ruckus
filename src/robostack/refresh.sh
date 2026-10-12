#!/bin/sh
# Refresh the vendored rosdep-to-conda mappings from RoboStack (MIT, see LICENSE).
# Each distro's repository keeps its own robostack.yaml. Run from anywhere:
#   sh src/robostack/refresh.sh
set -eu
cd "$(dirname "$0")"
for distro in humble jazzy kilted lyrical rolling; do
    curl -fsSL -o "$distro.yaml" \
        "https://raw.githubusercontent.com/RoboStack/ros-$distro/main/robostack.yaml"
done
curl -fsSL -o LICENSE https://raw.githubusercontent.com/RoboStack/ros-jazzy/main/LICENSE
