#!/bin/bash
# Root part of the Debian CI host. Run: sudo bash ops/ci-hosts/debian-setup.sh [finalize]
#   (no argument)  packages, user metro-ci, subuid/subgid, linger, slice limits, units,
#                  and the invoking user's ~/.ssh/id_rsa.pub as metro-ci's only SSH key.
#   finalize       removes metro-ci's SSH key and sets its shell to nologin.
set -euo pipefail
[ "$(id -u)" = 0 ] || { echo "run with sudo" >&2; exit 1; }
U=metro-ci
H=/home/$U

if [ "${1:-}" = finalize ]; then
  rm -f "$H/.ssh/authorized_keys"
  usermod --shell /usr/sbin/nologin "$U"
  echo "finalized: no SSH login for $U"; exit 0
fi

export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
# Rootless Docker needs uidmap, slirp4netns and a user D-Bus; the rest is what CI jobs
# would otherwise sudo apt-get install (metro-ci has no sudo).
apt-get install -y -qq --no-install-recommends \
  uidmap slirp4netns dbus-user-session \
  libicu76 git curl ca-certificates build-essential pkg-config xz-utils zstd unzip jq \
  python3 python3-yaml desktop-file-utils libglib2.0-bin shellcheck chromium file

if ! getent passwd "$U" >/dev/null; then
  useradd --system --user-group --create-home --home-dir "$H" --shell /bin/bash "$U"
fi
usermod --password '*' "$U"          # no password: key-only SSH, no su, no sudo
chmod 750 "$H"
for g in docker sudo; do gpasswd --delete "$U" "$g" >/dev/null 2>&1 || true; done

for f in /etc/subuid /etc/subgid; do
  grep -q "^$U:" "$f" || echo "$U:231072:65536" >> "$f"   # after wenzani's 100000-165535
done

key="$(getent passwd "${SUDO_USER:-wenzani}" | cut -d: -f6)/.ssh/id_rsa.pub"
install -d -o "$U" -g "$U" -m 700 "$H/.ssh"
install -o "$U" -g "$U" -m 600 "$key" "$H/.ssh/authorized_keys"

UID_N="$(id -u "$U")"
loginctl enable-linger "$U"

# One budget for the runner and the rootless dockerd/containers (both in user-UID.slice).
install -d "/etc/systemd/system/user-$UID_N.slice.d"
cat > "/etc/systemd/system/user-$UID_N.slice.d/50-metro-ci.conf" <<EOF
[Slice]
MemoryHigh=14G
MemoryMax=16G
MemorySwapMax=2G
CPUQuota=1200%
TasksMax=8192
EOF

cat > /etc/systemd/system/metro-ci-runner.service <<EOF
[Unit]
Description=Metro GitHub Actions runner (metro-ci-linux-docker)
Wants=network-online.target user@$UID_N.service
After=network-online.target user@$UID_N.service

[Service]
User=$U
Group=$U
Slice=user-$UID_N.slice
WorkingDirectory=$H/actions-runner
ExecStart=$H/actions-runner/run.sh
Environment=XDG_RUNTIME_DIR=/run/user/$UID_N
Environment=DOCKER_HOST=unix:///run/user/$UID_N/docker.sock
KillMode=process
KillSignal=SIGTERM
TimeoutStopSec=5min
Restart=always
RestartSec=10
NoNewPrivileges=yes
PrivateTmp=yes
ProtectSystem=full
ProtectHome=tmpfs
BindPaths=$H /run/user/$UID_N

[Install]
WantedBy=multi-user.target
EOF

# Starts the runner once debian-runner.sh has written its marker (no second sudo).
cat > /etc/systemd/system/metro-ci-runner.path <<EOF
[Unit]
Description=Start metro-ci-runner once configured

[Path]
PathExists=$H/actions-runner/.metro-configured
Unit=metro-ci-runner.service

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl start "user@$UID_N.service"
systemctl enable metro-ci-runner.service
systemctl enable --now metro-ci-runner.path
echo "done: uid $UID_N; next as $U: ssh $U@localhost bash -s < ops/ci-hosts/debian-runner.sh"
