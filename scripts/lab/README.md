# Phase 6 interop lab (birchtree <-> vanilla Mastodon 4.7.3)

Lab-only. Nothing here runs in production. The lab switches are off by default
in the server; these scripts turn them on for one private bridge network.

## What it proves

`phase6-interop.sh` runs the Follow family in both directions against a real
Mastodon 4.7.3 built from tag `8ff11c2b`, and prints PASS/FAIL per step:
WebFinger and actor discovery, inbound Follow with Accept, outbound Follow with
Accept (including a pending request on a locked account), outbound Undo, inbound
Undo, and inbound Reject. Each step reads the state on both sides. The result of
the last run is in `docs/logs/2026-10-17-phase-6-interop-run.out`, and birchtree's
federation log for that run is in `docs/logs/2026-10-17-phase-6-interop-birchtree.log`.

## Topology

| Piece | Address | Notes |
|---|---|---|
| Docker bridge `birchtree-lab` | 172.28.0.0/24, gateway 172.28.0.1 | the host is the gateway |
| Postgres 16 (`trust` auth) | 172.28.0.2 | `POSTGRES_HOST_AUTH_METHOD=trust` |
| Redis 7 | 172.28.0.3 | |
| Mastodon web (`birchtree-parity/mastodon:v4.7.3`) | 172.28.0.11 | `RAILS_ENV=production`, env from `mastodon.env` |
| Mastodon Sidekiq | 172.28.0.12 | same image |
| TLS proxy for Mastodon | 172.28.0.10 | `mastodon.lab` on 443 (TLS) and 80 (plain, for the crate's debug WebFinger) |
| TLS proxy for birchtree | 172.28.0.20 | `birchtree.lab` on 443, to the host on 8080 |
| birchtree | host, 0.0.0.0:8080 | `scripts/lab/start-birchtree.sh` |

Host `/etc/hosts`: `172.28.0.10 mastodon.lab` and `172.28.0.20 birchtree.lab`.
Mastodon containers get `--add-host` for both names.

## Bring-up (commands used on 2026-10-17)

1. `docker network create --subnet 172.28.0.0/24 --gateway 172.28.0.1 birchtree-lab`
2. Postgres and Redis at .2 and .3 (see the table).
3. Mastodon database: `docker run --rm --network birchtree-lab --env-file mastodon.env -e SSL_CERT_FILE=/certs/ca-bundle.pem -v <certs>:/certs:ro --add-host mastodon.lab:172.28.0.10 --add-host birchtree.lab:172.28.0.20 birchtree-parity/mastodon:v4.7.3 bin/rails db:prepare`
4. Mastodon web and Sidekiq with the same options, commands `bin/rails server -b 0.0.0.0 -p 3000` and `bundle exec sidekiq`.
5. nginx at .10 and .20 with the configs for `mastodon.lab` and `birchtree.lab`.
6. `alice` on Mastodon, through `bin/rails runner` (`User.new(...).save!(validate: false)` then `approve!`; the email domain check needs a real MX, which the lab does not have).
7. `bob` on birchtree: `birchtree --data-dir=$BT_DATA --public-url=https://birchtree.lab create-account --username=bob --password=... --email=...`.
8. Run `phase6-interop.sh`.

`mastodon.env` holds lab-generated secrets and is not in the repository. The lab CA
(`lab-ca.crt`) is passed to birchtree with `--federation-extra-ca`, not installed in
the host trust store.

## Environment variables

- `LAB`: the lab directory with `certs/` and `birchtree-phase6.log` (default `/root/parity/lab`).
- `BT_DATA`: birchtree's data directory, recreated by the run (default `/root/parity/birchtree-lab-data`).
