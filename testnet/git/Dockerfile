# A git server: bare repositories over ssh, as GitHub or Forgejo serve them
# for `git push`. One user, git, whose shell is git-shell and who logs in
# with the stack's key only. M51's `git push` from a pane on a box reaches
# it with the agent forwarded through `arugula --ssh`; see testnet/README.md.
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends openssh-server git \
 && rm -rf /var/lib/apt/lists/* /etc/ssh/ssh_host_* \
 && mkdir -p /run/sshd /srv/git \
 && useradd -m -s /usr/bin/git-shell git \
 && usermod -p '*' git \
 && git init -q --bare -b main /srv/git/repo.git \
 && chown -R git:git /srv/git
COPY sshd_config /etc/ssh/sshd_config
COPY entrypoint.sh /entrypoint.sh
EXPOSE 22
ENTRYPOINT ["/entrypoint.sh"]
