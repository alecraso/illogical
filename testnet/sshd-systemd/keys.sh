#!/bin/sh
# The same as sshd/entrypoint.sh, run by systemd before ssh.service.
set -eu
install -m 600 /keys/box-systemd_host_ed25519 /etc/ssh/ssh_host_ed25519_key
install -m 644 /keys/box-systemd_host_ed25519.pub /etc/ssh/ssh_host_ed25519_key.pub
install -d -m 700 -o illo -g illo /home/illo/.ssh
install -m 600 -o illo -g illo /keys/id_ed25519.pub /home/illo/.ssh/authorized_keys
install -m 644 /keys/known_hosts /etc/ssh/ssh_known_hosts
