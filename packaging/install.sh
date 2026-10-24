#!/bin/sh
# Installs mfd-bosun as a systemd service, from the unpacked release archive:
#   sudo ./install.sh
# Run it again to update: the binary and the unit are replaced, /etc/default/mfd-bosun is kept.
set -e
cd "$(dirname "$0")"

if [ "$(id -u)" -ne 0 ]; then
  echo "run as root: sudo $0" >&2
  exit 1
fi
if [ ! -d /run/systemd/system ]; then
  echo "no systemd here: run /usr/local/bin/mfd-bosun yourself" >&2
fi

install -m 755 mfd-bosun /usr/local/bin/mfd-bosun
if [ -e /etc/default/mfd-bosun ]; then
  echo "keeping /etc/default/mfd-bosun"
else
  # it may hold a password; not every distribution has /etc/default
  install -d /etc/default
  install -m 600 mfd-bosun.default /etc/default/mfd-bosun
fi

if [ -d /run/systemd/system ]; then
  install -m 644 mfd-bosun.service /etc/systemd/system/mfd-bosun.service
  systemctl daemon-reload
  systemctl enable mfd-bosun
  systemctl restart mfd-bosun
  echo "mfd-bosun is running: journalctl -u mfd-bosun -f shows its log and address"
fi
