#!/usr/bin/env bash
# Raise the workspace version by one build, cut a release, or set it outright.
#
#   scripts/bump-version.sh            0.1.4 -> 0.1.5   (every binary build)
#   scripts/bump-version.sh release    0.1.5 -> 0.2.6   (a release is a build too)
#   scripts/bump-version.sh 0.1.4      straight to 0.1.4
#
# The number reads major.release.build, the same scheme as the node repository:
# the third figure counts builds and never resets, so any binary traces back to
# the "build: version" commit that made it. The version lives in exactly one
# place, [workspace.package] in Cargo.toml; the member crates inherit it.
set -euo pipefail

cd "$(dirname "$0")/.."

current=$(awk '/^\[workspace\.package\]/{f=1} f && /^version = "/{gsub(/"/,"",$3); print $3; exit}' Cargo.toml)
if [[ -z "$current" ]]; then
    echo "cannot find the workspace version in Cargo.toml" >&2
    exit 1
fi

IFS=. read -r major minor patch <<<"$current"
if [[ -z "${patch:-}" ]]; then
    echo "workspace version '$current' is not major.release.build" >&2
    exit 1
fi
if [[ $# -ge 1 && "$1" == "release" ]]; then
    next="$major.$((minor + 1)).$((patch + 1))"
elif [[ $# -ge 1 ]]; then
    next="$1"
else
    next="$major.$minor.$((patch + 1))"
fi

if [[ "$next" == "$current" ]]; then
    echo "already at $current"
    exit 0
fi

python3 - "$current" "$next" <<'PY'
import sys
current, next_ = sys.argv[1], sys.argv[2]
out, in_workspace_package, changed = [], False, 0
for line in open("Cargo.toml"):
    if line.startswith("["):
        in_workspace_package = line.strip() == "[workspace.package]"
    if in_workspace_package and line.startswith(f'version = "{current}"'):
        line = line.replace(f'version = "{current}"', f'version = "{next_}"')
        changed += 1
    out.append(line)
if changed != 1:
    sys.exit(f"expected to rewrite exactly one version line, rewrote {changed}")
open("Cargo.toml", "w").write("".join(out))
PY

# Cargo.lock is refreshed by the next build on the rig, which fetches it back
# (scripts/remote-build.sh); this machine never runs cargo.
echo "$current -> $next"
