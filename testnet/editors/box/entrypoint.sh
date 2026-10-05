#!/bin/sh
# Keys from /keys (testnet/editors/.state, read-only), a forwarder from the
# box's address to the daemon's loopback port, then sshd in the foreground.
set -eu
install -m 600 /keys/box_host_ed25519 /etc/ssh/ssh_host_ed25519_key
install -m 644 /keys/box_host_ed25519.pub /etc/ssh/ssh_host_ed25519_key.pub
install -d -m 700 -o illo -g illo /home/illo/.ssh
install -m 600 -o illo -g illo /keys/id_ed25519.pub /home/illo/.ssh/authorized_keys
port="${DAEMON_PORT:?}"
socat "TCP-LISTEN:$port,bind=$(hostname -i | cut -d' ' -f1),fork,reuseaddr" "TCP:127.0.0.1:$port" &
exec /usr/sbin/sshd -D -e
