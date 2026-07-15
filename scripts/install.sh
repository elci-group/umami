#!/usr/bin/env bash
# Build and install umami system-wide. Run from the repo root.
set -euo pipefail

cargo build --release

sudo install -Dm755 target/release/umami /usr/local/sbin/umami
if [[ ! -f /etc/umami/umami.toml ]]; then
    sudo install -Dm644 config/umami.toml /etc/umami/umami.toml
fi
sudo install -Dm644 systemd/umami.service /etc/systemd/system/umami.service
sudo systemctl daemon-reload

cat <<'EOF'
Installed. Next steps:
  1. Edit /etc/umami/umami.toml — set tiers.umami_device to your flash
     device (find it with: ls -l /dev/disk/by-id/)
  2. Format + activate tiers once:  sudo umami setup --format
  3. Enable the daemon:             sudo systemctl enable --now umami.service
  4. Watch it work:                 journalctl -u umami -f
EOF
