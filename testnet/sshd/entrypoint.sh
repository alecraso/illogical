#!/bin/sh
# Install this box's host key and the stack's client key from /keys (the
# stack's .state, mounted read-only), then run sshd in the foreground.
set -eu
name="${TESTNET_NAME:?}"
install -m 600 "/keys/${name}_host_ed25519" /etc/ssh/ssh_host_ed25519_key
install -m 644 "/keys/${name}_host_ed25519.pub" /etc/ssh/ssh_host_ed25519_key.pub
install -d -m 700 -o illo -g illo /home/illo/.ssh
install -m 600 -o illo -g illo /keys/id_ed25519.pub /home/illo/.ssh/authorized_keys
exec /usr/sbin/sshd -D -e
