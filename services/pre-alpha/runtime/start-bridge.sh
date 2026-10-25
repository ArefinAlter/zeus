#!/bin/sh
# Wait for the daemon's socket, then start the bridge in hosted mode (see docker-compose.yml for its settings).
set -eu
i=0
while [ ! -S /run/zeus/zeus/zeusd.sock ] && [ $i -lt 60 ]; do i=$((i + 1)); sleep 1; done
exec zeus-web-bridge
