# box-systemd: box-bare's image with systemd as PID 1, logind and polkit, for
# S28's lifetime question (does `loginctl enable-linger` work without sudo?).
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends systemd systemd-sysv dbus dbus-user-session libpam-systemd polkitd openssh-server openssh-client procps \
 && rm -rf /var/lib/apt/lists/* /etc/ssh/ssh_host_* \
 && useradd -m -s /bin/bash illo \
 && usermod -p '*' illo \
 && systemctl mask getty@tty1.service console-getty.service systemd-firstboot.service
COPY sshd_config /etc/ssh/sshd_config
COPY keys.service /etc/systemd/system/testnet-keys.service
COPY keys.sh /usr/local/sbin/testnet-keys
RUN systemctl enable testnet-keys.service ssh.service
STOPSIGNAL SIGRTMIN+3
CMD ["/sbin/init"]
