#!/bin/sh
# Provision on first start (secrets and credentials, including the restricted gui.token the bridge uses), then run.
set -eu
umask 077
if [ ! -f /state/credentials/gui.token ]; then
  zeus daemon install >/dev/null
fi
mkdir -p /data/downloads/s
exec zeusd run
