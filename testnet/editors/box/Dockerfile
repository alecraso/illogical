# The machine VS Code reaches over Remote-SSH for M28's test: OpenSSH, one
# user (illo), and what the test's project needs there: Node for VS Code's
# JavaScript debugger, Python for the stand-in Claude Code, socat to hand the
# daemon's loopback port to the phone. illogicald arrives mounted at
# /opt/illogical. See testnet/editors/README.md.
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends openssh-server openssh-client procps nodejs python3 socat ca-certificates git tar gzip \
 && rm -rf /var/lib/apt/lists/* /etc/ssh/ssh_host_* \
 && mkdir -p /run/sshd \
 && useradd -m -s /bin/bash illo \
 && usermod -p '*' illo
COPY sshd_config /etc/ssh/sshd_config
COPY entrypoint.sh /entrypoint.sh
EXPOSE 22
ENTRYPOINT ["/entrypoint.sh"]
