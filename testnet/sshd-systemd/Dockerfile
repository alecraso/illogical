# box-systemd: box-bare's image with systemd as PID 1, logind and polkit, for
# S28's lifetime question (does `loginctl enable-linger` work without sudo?)
# and #26's reboot test (`docker restart`; python3 runs the fake ACP agent).
# The journal is kept on disk so it outlives a restart.
# systemd-binfmt is masked: the container is privileged and shares the
# kernel's binfmt_misc, so it would wipe the host's emulators (Docker
# Desktop's amd64 qemu, which `just desktop-linux x86_64` needs) and
# register python3's in their place.
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends systemd systemd-sysv dbus dbus-user-session libpam-systemd polkitd openssh-server openssh-client git python3 procps \
 && rm -rf /var/lib/apt/lists/* /etc/ssh/ssh_host_* \
 && useradd -m -s /bin/bash illo \
 && usermod -p '*' illo \
 && mkdir -p /var/log/journal \
 && systemctl mask getty@tty1.service console-getty.service systemd-firstboot.service \
      systemd-binfmt.service proc-sys-fs-binfmt_misc.automount proc-sys-fs-binfmt_misc.mount
COPY sshd_config /etc/ssh/sshd_config
COPY keys.service /etc/systemd/system/testnet-keys.service
COPY keys.sh /usr/local/sbin/testnet-keys
RUN systemctl enable testnet-keys.service ssh.service
STOPSIGNAL SIGRTMIN+3
CMD ["/sbin/init"]
