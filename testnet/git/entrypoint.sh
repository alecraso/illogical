#!/bin/sh
# The git server's host key and the stack's client key from /keys, then sshd.
set -eu
install -m 600 /keys/git_host_ed25519 /etc/ssh/ssh_host_ed25519_key
install -m 644 /keys/git_host_ed25519.pub /etc/ssh/ssh_host_ed25519_key.pub
install -d -m 700 -o git -g git /home/git/.ssh
install -m 600 -o git -g git /keys/id_ed25519.pub /home/git/.ssh/authorized_keys
exec /usr/sbin/sshd -D -e
