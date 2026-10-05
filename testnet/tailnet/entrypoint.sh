#!/bin/sh
# Keys as sshd/entrypoint.sh does (and the stack's client key for root, so
# the client can ssh to the box), then tailscaled, joined to headscale with
# the stack's preauthorized key, then sshd in the foreground.
set -eu
name="${TESTNET_NAME:?}"
install -m 600 "/keys/${name}_host_ed25519" /etc/ssh/ssh_host_ed25519_key
install -m 644 "/keys/${name}_host_ed25519.pub" /etc/ssh/ssh_host_ed25519_key.pub
install -d -m 700 -o illo -g illo /home/illo/.ssh
install -m 600 -o illo -g illo /keys/id_ed25519.pub /home/illo/.ssh/authorized_keys
install -m 644 /keys/known_hosts /etc/ssh/ssh_known_hosts
install -d -m 700 /root/.ssh
install -m 600 /keys/id_ed25519 /root/.ssh/id_ed25519
mkdir -p /var/lib/tailscale /var/run/tailscale
tailscaled --tun="${TS_TUN:-userspace-networking}" --statedir=/var/lib/tailscale \
  --socket=/var/run/tailscale/tailscaled.sock >/var/log/tailscaled.log 2>&1 &
for _ in $(seq 1 50); do tailscale status >/dev/null 2>&1 && break; [ -S /var/run/tailscale/tailscaled.sock ] && break; sleep 0.2; done
tailscale up --login-server=http://headscale:8080 --authkey="$(cat /keys/tailnet-authkey)" \
  --hostname="$name" --accept-dns=false
exec /usr/sbin/sshd -D -e
