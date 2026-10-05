#!/bin/sh
# Install this box's host key and the stack's client key from /keys (the
# stack's .state, mounted read-only), trust the stack's host keys (the git
# server's, for `git push` from the box), then run sshd in the foreground.
set -eu
name="${TESTNET_NAME:?}"
install -m 600 "/keys/${name}_host_ed25519" /etc/ssh/ssh_host_ed25519_key
install -m 644 "/keys/${name}_host_ed25519.pub" /etc/ssh/ssh_host_ed25519_key.pub
install -d -m 700 -o illo -g illo /home/illo/.ssh
install -m 600 -o illo -g illo /keys/id_ed25519.pub /home/illo/.ssh/authorized_keys
install -m 644 /keys/known_hosts /etc/ssh/ssh_known_hosts
exec /usr/sbin/sshd -D -e
