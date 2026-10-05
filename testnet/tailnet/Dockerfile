# A tailnet node for S28's comparison of ssh with the tailnet path: Debian
# with OpenSSH (user illo, as the ssh profile's boxes) and Tailscale, which
# joins the stack's headscale with a preauthorized key. TS_TUN picks
# tailscaled's mode: userspace-networking (a box: its netstack forwards
# tailnet connections to loopback) or tailscale0 (the client, which needs a
# real interface to dial the tailnet). See testnet/README.md.
FROM debian:bookworm-slim
ARG TARGETARCH
ARG TAILSCALE=1.102.4
RUN apt-get update \
 && apt-get install -y --no-install-recommends openssh-server openssh-client ca-certificates curl iproute2 iptables procps \
 && curl -fsSL "https://pkgs.tailscale.com/stable/tailscale_${TAILSCALE}_${TARGETARCH}.tgz" | tar xz -C /tmp \
 && mv /tmp/tailscale_*/tailscale /tmp/tailscale_*/tailscaled /usr/local/bin/ \
 && rm -rf /tmp/tailscale_* /var/lib/apt/lists/* /etc/ssh/ssh_host_* \
 && mkdir -p /run/sshd \
 && useradd -m -s /bin/bash illo \
 && usermod -p '*' illo
COPY sshd_config /etc/ssh/sshd_config
COPY entrypoint.sh /entrypoint.sh
EXPOSE 22
ENTRYPOINT ["/entrypoint.sh"]
