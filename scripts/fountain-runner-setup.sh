#!/usr/bin/env bash
# Make this machine a Fountain runner (M45): the root half. Run it once, as
# root through sudo, from the account that will look after the runner:
#
#   sudo bash scripts/fountain-runner-setup.sh \
#     --fountain "$(command -v fountain)" --node "$(node -p process.execPath)"
#
# Then, without root: `arugula fountain runner install` creates the
# runner's key, writes it for the `fountain` user and starts the unit.
#
# What it does:
#   - a `fountain` user and group (home /home/fountain, mode 0750), with the
#     invoking user ($SUDO_USER, or --user) in the group, so sandboxes can be
#     read and diffed without root;
#   - /home/fountain/sandboxes (2750: new files keep the group) as --root;
#   - /opt/fountain-node: a copy of node and npm (npx with it). Fountain's
#     runner needs bash, git, node, npm and npx on its PATH, and the user's
#     own node (nvm, mise) is in a home the `fountain` user can't read, so it
#     gets a root-owned copy of just node and npm (about 140 MB);
#   - /usr/local/bin/fountain: a copy of the CLI (run this again to upgrade);
#   - /etc/sudoers.d/arugula-fountain, checked with `visudo -cf`: the user
#     may run bash as `fountain` (a shell in a sandbox, and writing its key),
#     and as root `systemctl start|stop|restart|status fountain-runner`,
#     nothing else;
#   - /etc/systemd/system/fountain-runner.service (User=fountain,
#     Restart=always, UMask=0027 so the group can read; ProtectProc=invisible,
#     so its agents can't read other users' processes' command lines in
#     /proc; systemd 247 or later; IPAddressDeny=localhost, so its agents
#     can't reach services on this machine's loopback, like arugulad,
#     except systemd-resolved's DNS stub), enabled. The unit is the one source of
#     truth: an interim drop-in (fountain-runner.service.d/10-protect-proc.conf)
#     is removed, with its directory if that leaves it empty. It starts
#     once its key exists (ConditionPathExists), so at boot without one it's
#     skipped rather than failing in a loop.
#
# Options:
#   --user USER       who gets the group and the sudoers rule [$SUDO_USER]
#   --name NAME       the runner's name [this host's short name, lowercased]
#   --fountain PATH   the fountain CLI to copy [`command -v fountain`]
#   --node PATH       the node executable whose node and npm to copy
#                     [`command -v node`]
#   --allow-loopback  let the runner's agents use loopback (a dev server they
#                     start and test, say): no IPAddressDeny=localhost
#   --dry-run DIR     write every file under DIR instead of /, print the
#                     commands instead of running them; needs no root
#   --uninstall       undo it all; the user, its home and the sandboxes stay
#   --purge           with --uninstall: remove the user, its home and every
#                     sandbox too
set -euo pipefail

say() { printf '%s\n' "$*"; }
die() { printf 'fountain-runner-setup: %s\n' "$*" >&2; exit 1; }

user=${SUDO_USER:-}
name=
fountain=
node=
root=
dry=
uninstall=
purge=
loopback=

while [ $# -gt 0 ]; do
  case "$1" in
    --user) user=${2:?}; shift 2 ;;
    --name) name=${2:?}; shift 2 ;;
    --fountain) fountain=${2:?}; shift 2 ;;
    --node) node=${2:?}; shift 2 ;;
    --allow-loopback) loopback=1; shift ;;
    --dry-run) dry=1; root=${2:?}; shift 2 ;;
    --uninstall) uninstall=1; shift ;;
    --purge) purge=1; shift ;;
    -h | --help) sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'; exit 0 ;;
    *) die "unknown argument: $1 (see --help)" ;;
  esac
done

[ -n "$purge" ] && [ -z "$uninstall" ] && die "--purge goes with --uninstall"
[ -n "$dry" ] || [ "$(id -u)" = 0 ] || die "run it as root: sudo bash $0 ..."

svc="fountain-runner"
home=/home/fountain
sandboxes=$home/sandboxes
node_dir=/opt/fountain-node
bin=/usr/local/bin/fountain
unit=/etc/systemd/system/$svc.service
sudoers=/etc/sudoers.d/arugula-fountain
# The rule's name before the rename (#505): replaced by the new one.
old_sudoers=/etc/sudoers.d/illogical-fountain

# Run a command, or print it in a dry run.
run() {
  if [ -n "$dry" ]; then
    say "+ $*"
  else
    "$@"
  fi
}

# The same, carrying on if it fails (undoing what may not be there).
run_ok() {
  run "$@" || say "  (failed: $*; carrying on)"
}

# Where a file goes: under DIR in a dry run.
at() { printf '%s%s' "$root" "$1"; }

# Loopback is shared by every account here: the runner's agents stay off
# it (the services on it, arugulad among them, aren't theirs), but for
# systemd-resolved's stub, which DNS goes through.
render_loopback() {
  if [ -n "$loopback" ]; then
    printf '# --allow-loopback: its agents may reach services on this machine'"'"'s loopback.'
  else
    printf '%s\n' "# Services on this machine's loopback aren't the runner's (--allow-loopback to allow)." \
      "IPAddressDeny=localhost" "IPAddressAllow=127.0.0.53 127.0.0.54"
  fi
}

render_unit() {
  cat <<EOF
# Written by Arugula's scripts/fountain-runner-setup.sh; run it again to change it.
[Unit]
Description=Fountain runner $name (sandboxes for agents on the runner provider)
After=network-online.target
Wants=network-online.target
ConditionPathExists=$home/.fountain/credentials

[Service]
Type=simple
User=fountain
Group=fountain
UMask=0027
WorkingDirectory=$home
Environment=HOME=$home
Environment=PATH=$node_dir/bin:/usr/local/bin:/usr/bin:/bin
ExecStart=$bin runner --name $name --root $sandboxes
Restart=always
RestartSec=10
NoNewPrivileges=yes
PrivateTmp=yes
# The runner's agents see only their own processes in /proc: not other
# users' command lines (an MCP config on claude's argv, say). Not
# ProcSubset=pid: it hides /proc/cpuinfo and meminfo (node's os.cpus()).
ProtectProc=invisible
InaccessiblePaths=-$user_home -/run/user/$user_uid
$(render_loopback)

[Install]
WantedBy=multi-user.target
EOF
}

render_sudoers() {
  local sc
  sc=$(command -v systemctl || echo /usr/bin/systemctl)
  cat <<EOF
# Written by Arugula's scripts/fountain-runner-setup.sh (--uninstall removes it).
# A shell as the Fountain runner's user (a sandbox's shell, and its key):
$user ALL=(fountain) NOPASSWD: /bin/bash
# The runner's unit, and nothing else as root:
$user ALL=(root) NOPASSWD: $sc start $svc, $sc stop $svc, $sc restart $svc, $sc status $svc
EOF
}

# The interim hardening drop-in, now in the unit itself.
dropin_dir=/etc/systemd/system/$svc.service.d
drop_interim() {
  run rm -f "$(at "$dropin_dir/10-protect-proc.conf")"
  if [ -d "$(at "$dropin_dir")" ]; then
    run rmdir --ignore-fail-on-non-empty "$(at "$dropin_dir")"
  fi
}

valid_name() { [[ "$1" =~ ^[a-z_][a-z0-9_.-]*$ ]]; }

[ -n "$user" ] || die "no user: run it through sudo, or pass --user"
valid_name "$user" || die "not a user name: $user"
[ "$user" != root ] || die "run it through sudo from your own account (or pass --user)"
if [ -n "$dry" ] && ! id "$user" >/dev/null 2>&1; then
  user_home=/home/$user
  user_uid=1000
else
  user_home=$(getent passwd "$user" | cut -d: -f6)
  user_uid=$(id -u "$user")
fi
[ -n "$user_home" ] || die "no home for $user"

if [ -n "$uninstall" ]; then
  say "Removing the Fountain runner setup."
  run_ok systemctl disable --now "$svc"
  run rm -f "$(at "$unit")" "$(at "$sudoers")" "$(at "$old_sudoers")" "$(at "$bin")"
  drop_interim
  run rm -rf "$(at "$node_dir")"
  run systemctl daemon-reload
  if [ -n "$purge" ]; then
    run_ok userdel --remove fountain
    run_ok groupdel fountain
    say "Removed the fountain user, its home and every sandbox."
  else
    run_ok gpasswd --delete "$user" fountain
    say "Kept the fountain user and $sandboxes (--purge removes them)."
  fi
  exit 0
fi

[ -n "$name" ] || name=$(hostname -s | tr '[:upper:]' '[:lower:]')
valid_name "$name" || die "not a runner name: $name"

[ -n "$fountain" ] || fountain=$(command -v fountain || true)
[ -n "$fountain" ] || die "no fountain CLI: pass --fountain \"\$(command -v fountain)\""
fountain=$(readlink -f "$fountain")
[ -x "$fountain" ] || die "not an executable: $fountain"

[ -n "$node" ] || node=$(command -v node || true)
[ -n "$node" ] || die "no node: pass --node \"\$(node -p process.execPath)\""
node=$(readlink -f "$node")
[ -x "$node" ] || die "not an executable: $node"
node_prefix=$(dirname "$(dirname "$node")")
npm_dir=$node_prefix/lib/node_modules/npm
[ -f "$npm_dir/bin/npm-cli.js" ] || die "no npm beside $node (looked for $npm_dir); pass a node from an install with npm"
command -v git >/dev/null || die "the runner needs git on this machine"

say "Fountain runner '$name': user fountain (with $user in its group), sandboxes in $sandboxes."
[ -n "$dry" ] && say "Dry run: files under $root, commands printed."

if [ -n "$dry" ]; then
  run groupadd --system fountain
  run useradd --system --gid fountain --home-dir "$home" --create-home --shell /bin/bash fountain
elif ! id fountain >/dev/null 2>&1; then
  getent group fountain >/dev/null || groupadd --system fountain
  useradd --system --gid fountain --home-dir "$home" --create-home --shell /bin/bash fountain
fi
run usermod --append --groups fountain "$user"
if [ -n "$dry" ]; then
  mkdir -p "$(at "$sandboxes")" "$(at "$home/.fountain")"
fi
run install -d -m 0750 -o fountain -g fountain "$(at "$home")"
run install -d -m 0700 -o fountain -g fountain "$(at "$home/.fountain")"
run install -d -m 2750 -o fountain -g fountain "$(at "$sandboxes")"

# node and npm, root-owned and readable by everyone.
run rm -rf "$(at "$node_dir")"
mkdir -p "$(at "$node_dir/bin")" "$(at "$node_dir/lib/node_modules")"
run install -m 0755 "$node" "$(at "$node_dir/bin/node")"
run cp -R "$npm_dir" "$(at "$node_dir/lib/node_modules/npm")"
ln -sfn ../lib/node_modules/npm/bin/npm-cli.js "$(at "$node_dir/bin/npm")"
ln -sfn ../lib/node_modules/npm/bin/npx-cli.js "$(at "$node_dir/bin/npx")"
run chmod -R a+rX,go-w "$(at "$node_dir")"

mkdir -p "$(at /usr/local/bin)"
run install -m 0755 "$fountain" "$(at "$bin")"

# The sudoers rule, checked before it's put in place.
mkdir -p "$(at /etc/sudoers.d)"
tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT
render_sudoers >"$tmp"
if command -v visudo >/dev/null; then
  visudo -cf "$tmp" >/dev/null || die "visudo refused the sudoers rule (left out): $(cat "$tmp")"
elif [ -z "$dry" ]; then
  die "no visudo to check the sudoers rule with"
fi
install -m 0440 "$tmp" "$(at "$sudoers")"
[ -n "$dry" ] || chown root:root "$sudoers"
[ ! -e "$(at "$old_sudoers")" ] || run rm -f "$(at "$old_sudoers")"

mkdir -p "$(at /etc/systemd/system)"
render_unit >"$(at "$unit")"
chmod 0644 "$(at "$unit")"
drop_interim
run systemctl daemon-reload
run systemctl enable "$svc"
# Running already (this is an upgrade): pick up the new binary and unit.
run systemctl try-restart "$svc"

say ""
say "Done. Now, as $user (no root): arugula fountain runner install"
say "It writes the runner's key to $home/.fountain/credentials and starts $svc."
say "$user is in the fountain group from its next login: processes started before"
say "(a shell, arugulad's user manager) don't have it until then or a reboot."
[ -n "$dry" ] && say "(Dry run: nothing outside $root changed.)"
exit 0
