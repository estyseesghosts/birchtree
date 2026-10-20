#!/usr/bin/env bash
# Take the phase 6 interop lab down. Removes the lab's /etc/hosts lines.
set -uo pipefail
for p in $(pgrep -x birchtree); do kill "$p"; done
docker rm -f bt-proxy-birchtree bt-proxy-mastodon bt-sidekiq bt-mastodon bt-redis bt-pg >/dev/null 2>&1
docker network rm birchtree-lab >/dev/null 2>&1
sed -i '/^172\.28\.0\.10 mastodon\.lab$/d; /^172\.28\.0\.20 birchtree\.lab$/d' /etc/hosts
echo "lab down"
