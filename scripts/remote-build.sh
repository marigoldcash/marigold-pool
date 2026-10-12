#!/usr/bin/env bash
#
# Build and test on the big rig (pve3) instead of here.
#
#   scripts/remote-build.sh                  # release build of marigold-pool
#   CHECK=1 scripts/remote-build.sh          # cargo check --workspace --tests
#   TEST=1  scripts/remote-build.sh          # cargo test --workspace
#   LINT=1  scripts/remote-build.sh          # cargo fmt --check, then clippy -D warnings
#   FMT=1   scripts/remote-build.sh          # cargo fmt on the rig, formatted .rs files copied back
#   NO_BUMP=1 scripts/remote-build.sh        # rebuild the same source on purpose
#   CARGO_FLAGS="--no-default-features --features chain-kaspa" TEST=1 scripts/remote-build.sh
#
# Mirrors the working tree to a build directory on the rig, runs cargo there,
# and for a release build copies the binary back into ./target/release. The
# remote target/ directory persists between runs, so after the first one these
# are incremental. Cargo.lock is fetched back every time, because resolution
# happens on the rig and the lock file is committed.
#
# Only a release build of the binary bumps the version: CHECK, TEST, LINT and FMT
# produce nothing that anyone could be holding.
set -euo pipefail

HOST="${MARIGOLD_BUILD_HOST:-pve3}"
KEY="${MARIGOLD_BUILD_KEY:-$HOME/.ssh/marigold_deploy}"
REMOTE_DIR="${MARIGOLD_BUILD_DIR:-/opt/marigold-build/pool-remote-build}"
SSH="ssh -i $KEY -o BatchMode=yes"
FLAGS="${CARGO_FLAGS:-}"

cd "$(git rev-parse --show-toplevel)"

if [ -z "${CHECK:-}${TEST:-}${LINT:-}${FMT:-}" ] && [ -z "${NO_BUMP:-}" ]; then
  echo "→ $(scripts/bump-version.sh)"
fi

echo "→ syncing working tree to $HOST:$REMOTE_DIR"
$SSH "$HOST" "mkdir -p $REMOTE_DIR"
rsync -a --delete --info=stats1 \
  --exclude '/target' --exclude '/.git' --exclude '/.claude' \
  -e "$SSH" ./ "$HOST:$REMOTE_DIR/"

fetch_lock() {
  rsync -a -e "$SSH" "$HOST:$REMOTE_DIR/Cargo.lock" ./Cargo.lock 2>/dev/null || true
}

if [ -n "${FMT:-}" ]; then
  echo "→ cargo fmt on $HOST"
  # shellcheck disable=SC2029
  $SSH "$HOST" "cd $REMOTE_DIR && cargo fmt --all"
  rsync -a -e "$SSH" --include='*/' --include='*.rs' --exclude='*' "$HOST:$REMOTE_DIR/crates/" crates/
  echo "✓ formatted; review with git diff"
  exit 0
fi

if [ -n "${LINT:-}" ]; then
  echo "→ fmt and clippy on $HOST"
  # shellcheck disable=SC2029
  $SSH "$HOST" "cd $REMOTE_DIR && set -o pipefail && cargo fmt --all --check && cargo clippy --workspace --tests $FLAGS -- -D warnings 2>&1 | tail -60" || { fetch_lock; echo "✗ lint failed" >&2; exit 1; }
  fetch_lock
  echo "✓ fmt and clippy clean"
  exit 0
fi

if [ -n "${CHECK:-}" ]; then
  echo "→ cargo check on $HOST"
  # shellcheck disable=SC2029
  $SSH "$HOST" "cd $REMOTE_DIR && set -o pipefail && cargo check --workspace --tests $FLAGS 2>&1 | tail -40" || { fetch_lock; echo "✗ check failed" >&2; exit 1; }
  fetch_lock
  echo "✓ check passed"
  exit 0
fi

if [ -n "${TEST:-}" ]; then
  echo "→ cargo test on $HOST"
  # shellcheck disable=SC2029
  $SSH "$HOST" "cd $REMOTE_DIR && set -o pipefail && cargo test --workspace $FLAGS 2>&1 | tail -80" || { fetch_lock; echo "✗ tests failed" >&2; exit 1; }
  fetch_lock
  echo "✓ tests passed"
  exit 0
fi

echo "→ cargo build --release --bin marigold-pool $FLAGS"
# `set -o pipefail` on the far side: without it a failed compile comes back as
# the tail's exit code, the old binary is fetched, and the script says "done".
# shellcheck disable=SC2029
$SSH "$HOST" "cd $REMOTE_DIR && set -o pipefail && cargo build --release --jobs \$(nproc) --bin marigold-pool $FLAGS 2>&1 | tail -30" || {
  fetch_lock
  echo "✗ the build failed — nothing fetched" >&2
  exit 1
}
fetch_lock

echo "→ fetching the binary"
mkdir -p target/release
rsync -a --info=name -e "$SSH" "$HOST:$REMOTE_DIR/target/release/marigold-pool" target/release/marigold-pool
echo "✓ done — target/release/marigold-pool  ($(awk '/^\[workspace\.package\]/{f=1} f && /^version = "/{gsub(/"/,"",$3); print $3; exit}' Cargo.toml))"
