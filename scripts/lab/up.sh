#!/usr/bin/env bash
# Bring up the phase 6 interop lab (see scripts/lab/README.md). Lab-only.
# Needs: docker, the Mastodon image birchtree-parity/mastodon:v4.7.3 (built from tag 8ff11c2b),
# and $LAB with mastodon.env, nginx-mastodon-lab.conf, nginx-birchtree.conf and certs/.
# Idempotent: existing containers and the network are reused.
set -euo pipefail
LAB="${LAB:-/root/parity/lab}"
IMG="${MASTODON_IMAGE:-birchtree-parity/mastodon:v4.7.3}"
HERE="$(cd "$(dirname "$0")" && pwd)"
run_if_absent() { local name="$1"; shift; docker ps -a --format '{{.Names}}' | grep -qx "$name" || "$@" >/dev/null; }

docker network inspect birchtree-lab >/dev/null 2>&1 || docker network create --subnet 172.28.0.0/24 --gateway 172.28.0.1 birchtree-lab >/dev/null
run_if_absent bt-pg docker run -d --name bt-pg --network birchtree-lab --ip 172.28.0.2 -e POSTGRES_HOST_AUTH_METHOD=trust postgres:16-alpine
run_if_absent bt-redis docker run -d --name bt-redis --network birchtree-lab --ip 172.28.0.3 redis:7-alpine
sleep 8
ARGS=(--network birchtree-lab --env-file "$LAB/mastodon.env" -e SSL_CERT_FILE=/certs/ca-bundle.pem -v "$LAB/certs:/certs:ro" --add-host mastodon.lab:172.28.0.10 --add-host birchtree.lab:172.28.0.20)
docker run --rm "${ARGS[@]}" "$IMG" bin/rails db:prepare >"$LAB/db-prepare.log" 2>&1 || { echo "db:prepare failed, see $LAB/db-prepare.log"; exit 1; }
run_if_absent bt-mastodon docker run -d --name bt-mastodon "${ARGS[@]}" --ip 172.28.0.11 "$IMG" bin/rails server -b 0.0.0.0 -p 3000
run_if_absent bt-sidekiq docker run -d --name bt-sidekiq "${ARGS[@]}" --ip 172.28.0.12 "$IMG" bundle exec sidekiq
run_if_absent bt-proxy-mastodon docker run -d --name bt-proxy-mastodon --network birchtree-lab --ip 172.28.0.10 -v "$HERE/nginx-mastodon.conf:/etc/nginx/nginx.conf:ro" -v "$LAB/certs:/certs:ro" nginx:alpine
run_if_absent bt-proxy-birchtree docker run -d --name bt-proxy-birchtree --network birchtree-lab --ip 172.28.0.20 -v "$HERE/nginx-birchtree.conf:/etc/nginx/nginx.conf:ro" -v "$LAB/certs:/certs:ro" nginx:alpine
grep -q "mastodon.lab" /etc/hosts || printf '172.28.0.10 mastodon.lab\n172.28.0.20 birchtree.lab\n' >> /etc/hosts
echo "lab up: mastodon.lab and birchtree.lab on the birchtree-lab network; birchtree runs on the host (scripts/lab/start-birchtree.sh)"
