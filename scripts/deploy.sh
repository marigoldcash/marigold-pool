#!/usr/bin/env bash
#
# Put marigold-pool on this machine: the program, its service unit, its settings
# file and its wallet password file. It starts nothing and enables nothing; the
# last thing it does is print the steps that are the operator's.
#
#   scripts/deploy.sh [path/to/marigold-pool]        # default: target/release/marigold-pool
#
# Run it as root on the machine that runs the node. What it does:
#
#   * copies the program to /opt/marigold/bin/marigold-pool (the old one is kept
#     beside it as marigold-pool.prev)
#   * installs deploy/marigold-pool.service into /etc/systemd/system
#   * installs deploy/pool.toml as /etc/marigold/pool.toml, but only if that file
#     is not there yet: a settings file somebody has edited is never overwritten
#   * makes /var/lib/marigold/pool, owned by the marigold user and private to it
#   * makes /etc/marigold/pool.env with a freshly generated wallet password, root's
#     and readable by nobody else, but only if it is not there yet
#
# To rehearse without touching the machine, set DEPLOY_ROOT to an empty directory:
# everything is then placed inside it, and no user, owner or service is involved.
set -euo pipefail

cd "$(dirname "$0")/.."

ROOT="${DEPLOY_ROOT:-}"
BINARY="${1:-target/release/marigold-pool}"
SERVICE_USER="marigold"

BIN_DIR="$ROOT/opt/marigold/bin"
BIN="$BIN_DIR/marigold-pool"
UNIT_DIR="$ROOT/etc/systemd/system"
CONF_DIR="$ROOT/etc/marigold"
DATA_DIR="$ROOT/var/lib/marigold/pool"

say() { printf '%s\n' "$*"; }
die() { printf 'Stopped: %s\n' "$*" >&2; exit 1; }

# ---- before touching anything -------------------------------------------------------------

[ -f "$BINARY" ] || die "$BINARY is not there. Build it first with scripts/remote-build.sh, which leaves it in target/release."
[ -x "$BINARY" ] || die "$BINARY is not an executable program."
VERSION_LINE="$("$BINARY" --version 2>&1)" || die "$BINARY does not run: $VERSION_LINE"
case "$VERSION_LINE" in
  marigold-pool\ *) ;;
  *) die "$BINARY printed \"$VERSION_LINE\" instead of its version, so it does not look like marigold-pool." ;;
esac
[ -f deploy/marigold-pool.service ] || die "deploy/marigold-pool.service is missing."
[ -f deploy/pool.toml ] || die "deploy/pool.toml is missing."
command -v openssl >/dev/null || die "openssl is needed to make the wallet password and was not found."

if [ -z "$ROOT" ]; then
  [ "$(id -u)" -eq 0 ] || die "run this as root: the files it places belong to root."
  id -u "$SERVICE_USER" >/dev/null 2>&1 || die "the $SERVICE_USER user does not exist here. The faucet's setup makes it; run that first."
fi

# ---- the program --------------------------------------------------------------------------

mkdir -p "$BIN_DIR" "$UNIT_DIR" "$CONF_DIR" "$DATA_DIR"
if [ -f "$BIN" ]; then
  cp -p "$BIN" "$BIN.prev"
  say "kept the old program as $BIN.prev"
fi
# Copy beside it and rename into place, so a running copy is never overwritten.
install -m 0755 "$BINARY" "$BIN.new"
mv -f "$BIN.new" "$BIN"
say "installed $BIN ($VERSION_LINE)"

# ---- the unit -----------------------------------------------------------------------------

install -m 0644 deploy/marigold-pool.service "$UNIT_DIR/marigold-pool.service"
say "installed $UNIT_DIR/marigold-pool.service"

# ---- the settings -------------------------------------------------------------------------

if [ -e "$CONF_DIR/pool.toml" ]; then
  say "kept your settings file $CONF_DIR/pool.toml as it is"
  if ! cmp -s deploy/pool.toml "$CONF_DIR/pool.toml"; then
    say "  (it differs from the one shipped with this version; deploy/pool.toml has the current defaults to compare with)"
  fi
else
  install -m 0644 deploy/pool.toml "$CONF_DIR/pool.toml"
  say "installed $CONF_DIR/pool.toml (its pool address is blank until you have run init)"
fi

# ---- the wallet password ------------------------------------------------------------------

if [ -e "$CONF_DIR/pool.env" ]; then
  say "kept the wallet password file $CONF_DIR/pool.env"
else
  ( umask 077; printf 'MARIGOLD_POOL_WALLET_SECRET=%s\n' "$(openssl rand -base64 24)" > "$CONF_DIR/pool.env" )
  chmod 600 "$CONF_DIR/pool.env"
  say "made $CONF_DIR/pool.env with a new wallet password (root only)"
fi

# ---- the data directory -------------------------------------------------------------------

chmod 0700 "$DATA_DIR"
if [ -z "$ROOT" ]; then
  chown "$SERVICE_USER:$SERVICE_USER" "$DATA_DIR"
  chown root:root "$CONF_DIR/pool.env"
fi
say "data directory $DATA_DIR is ready"

# ---- what is left is yours ----------------------------------------------------------------

cat <<'NEXT'

Everything is in place. Nothing has been started. The rest is yours:

  1. Make the pool wallet. This prints the pool's address and its twelve recovery
     words, once. Write the words down; the pool does not keep them.

       systemd-run --quiet --wait --pipe --collect --uid=marigold \
         --property=EnvironmentFile=/etc/marigold/pool.env \
         /opt/marigold/bin/marigold-pool init --data-dir /var/lib/marigold/pool --network testnet-10

  2. Put that address into /etc/marigold/pool.toml, on the line   address = ""

  3. Check the settings. This starts nothing.

       /opt/marigold/bin/marigold-pool check --config /etc/marigold/pool.toml

  4. Open the stratum ports in the firewall, by hand as on the other hosts:
     TCP 5570 for CPU miners and TCP 5571 for ASICs. (The rig's own bridge keeps its ports.)

  5. Start the pool.

       systemctl daemon-reload
       systemctl enable --now marigold-pool

  6. Watch it come up.

       journalctl -u marigold-pool -f
       curl -s http://127.0.0.1:8788/health

  7. Point a CPU wallet at it (from any machine):

       marigold-cli mine start --pool <this machine>:5570

     and point an ASIC at <this machine>:5571 only when you decide to.

To pay by hand once miners are owed something. A wallet can be open in only one
program at a time, so the pool is stopped for the moment this takes:

       systemctl stop marigold-pool
       systemd-run --quiet --wait --pipe --collect --uid=marigold \
         --property=EnvironmentFile=/etc/marigold/pool.env \
         /opt/marigold/bin/marigold-pool payout --now --config /etc/marigold/pool.toml
       systemctl start marigold-pool

(Add  --min-payout-coins 0.5  to pay balances smaller than the settings file's minimum.)
NEXT
