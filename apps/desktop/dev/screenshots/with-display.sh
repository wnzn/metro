#!/bin/sh
# Runs a command on a private Xvfb display inside the screenshot container.
# (xvfb-run hangs as the container's PID 1, so start Xvfb directly.)
set -eu
num="${DISPLAY_NUM:-99}"
Xvfb ":$num" -screen 0 "${SCREEN:-1280x800x24}" -nolisten tcp >/tmp/xvfb.log 2>&1 &
xvfb=$!
trap 'kill "$xvfb" 2>/dev/null || true' EXIT INT TERM
i=0
while [ ! -e "/tmp/.X11-unix/X$num" ]; do
  i=$((i + 1))
  if [ "$i" -gt 100 ]; then echo "Xvfb did not start" >&2; cat /tmp/xvfb.log >&2; exit 1; fi
  sleep 0.1
done
export DISPLAY=":$num"
exec "$@"
