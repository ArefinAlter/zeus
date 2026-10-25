#!/bin/sh
# Every ten minutes: delete sandbox files older than 24 hours, then sandbox folders left empty for 24 hours.
set -u
while true; do
  if [ -d /data/downloads/s ]; then
    find /data/downloads/s -mindepth 2 -type f -mmin +1440 -delete 2>/dev/null
    find /data/downloads/s -mindepth 1 -type d -empty -mmin +1440 -delete 2>/dev/null
  fi
  sleep 600
done
