# birchtree

> **Hinweis:** Dieses Projekt bewegt sich schnell, und dieses Dokument kann rasch veraltet sein. Die englische README im Projektstamm ist in der Regel aktueller.

birchtree ist ein **fediverse-Server mit Mastodon-API-Kompatibilität** als
einzelner Prozess, geschrieben in Rust. Er reimplementiert
[Mastodon **4.7.3**](https://github.com/mastodon/mastodon/blob/v4.7.3/CHANGELOG.md)
(API v7), gestützt auf eingebettetes `redb` und lokale Medienablage.
Föderation spricht ActivityPub über
[`activitypub-federation-rust`](https://github.com/LemmyNet/activitypub-federation-rust)
plus birchtrees eigene Richtlinien- und Zustellregeln.

Er ist bewusst **schlank im Betrieb**: kein Postgres, kein Redis, kein
Message-Broker. Ein Binary, ein Datenverzeichnis (`redb`-Datei + Medien),
fertig.

birchtree bringt kein Frontend mit. Richte einen beliebigen
Mastodon-kompatiblen Client darauf ein — siehe
[keystone](https://github.com/estyseesghosts/keystone) (in Entwicklung). Das
offizielle Mastodon-Frontend wurde bisher nicht dagegen getestet. YMMV.

> **Status (2026-10-20):** lokal nutzbar und im Labor föderierend
> (`cargo test --release`: 349 bestanden / 0 fehlgeschlagen). Konten, Status,
> Beziehungen, Medien, Timelines, Benachrichtigungen/Marker/Listen/Filter,
> Streaming (SSE + WebSocket) und Kernföderation sind implementiert und im
> Labor gegen echtes Mastodon 4.7.3 verifiziert. Unterhaltungen, Web Push,
> Admin-API, `/api/web/*`, Transkodierung, Volltextsuche und Mehrknotenbetrieb
> sind geplant, nicht gebaut. Nur in simulierten Umgebungen und einem privaten
> Labor getestet — nie gegen das öffentliche Fediverse.

---

## Inhaltsverzeichnis

- [Was es heute kann](#what-it-does-today)
- [Was es nicht kann](#what-it-does-not-do)
- [Bekannte Lücken und Ecken](#known-gaps-and-rough-edges)
- [Stand der Dinge](#where-things-stand)
- [Eigenen Server betreiben](#run-your-own-server)
  - [Voraussetzungen](#prerequisites)
  - [Bauen](#build-it)
  - [Starten](#start-it)
  - [Erstes Konto anlegen](#create-your-first-account)
  - [Anmelden und posten](#log-in-and-post)
- [Tägliche Nutzung](#use-it-day-to-day)
- [Womit es spricht](#what-it-talks-to)
- [Projektstruktur](#project-layout)
- [Selbst testen](#testing-it-yourself)
- [Mitwirken & weiterlesen](#contributing--reading-more)
- [Lizenz](#license)

---

## Was es heute kann

Alles in diesem Abschnitt läuft auf dem Einzelknoten-Server und ist durch
ausgeführte Tests abgedeckt. Nicht implementierte Routen antworten mit einem
sauberen `404` — niemals mit erfundenen Daten — mit den unten genannten
Ausnahmen (Endpunkte, die korrekterweise leer antworten, weil ihre
Datenquelle noch nicht existiert). Der genaue Vertrag steht in
`docs/endpoint-manifest.md` plus `docs/compatibility-profile.md`
(Abweichungsregister D1–D18).

**Konten, Auth und Registrierung:**

- OAuth2-App-Registrierung (`POST /api/v1/apps`), Authorization-Code-Flow mit
  PKCE S256, Client-Credentials-Apps, Token-Widerruf, Scope-Prüfung pro Route,
  serverseitig gerenderte Login-/Zustimmungsseiten.
- Registrierungsrichtlinie (offen/geschlossen), CLI-Bootstrap-Konto.
  E-Mail-Versand ist ein Logging-Stub — keine Durchsetzung.
- Mastodon-kompatible Dezimal-IDs: 64-Bit zeitstempelbasierte IDs, serialisiert
  als Dezimalstrings; intern numerischer Vergleich, niemals lexikografisch.

**Status und Interaktionen:**

- Posten, Anzeigen, Bearbeiten (mit Verlauf), Löschen, Antworten + Kontext,
  Reblogs, Favoriten, Lesezeichen, Pins (Limit 5),
  `favourited_by`/`reblogged_by`, Quellansicht, Mehrfachabruf,
  `Idempotency-Key`-Wiedergabe (1 Stunde Aufbewahrung).
- Zitate als lokale Zustandsmaschine: Richtlinie
  (`public`/`followers`/`nobody`), Annehmen/Widerrufen,
  Interaktionsrichtlinien-Endpunkt, `quotes_count`.
- Lokale Umfragen (Erstellen/Abstimmen/Ablauf, verborgene Summen) und geplante
  Status (mit `<5 min`-422-Parität).
- Textlimit für Status standardmäßig 500 Zeichen (Mastodon-Parität),
  konfigurierbar bis 3000 via `--max-status-chars=`.

**Beziehungen:**

- Folgen (gesperrte Konten → ausstehende Anfragen mit
  Annehmen/Ablehnen), Entfolgen, Aus-Followern-Entfernen, Blockieren,
  Stummschalten (mit Ablauf), Domain-Blocks (Upstream-v4.7.3-Form),
  Hervorhebungen, Kontonotizen, Follower-/Folge-Listen,
  Beziehungsaggregate pro Betrachter.

**Medien:**

- v1-/v2-Multipart-Upload mit MIME-Sniffing, Verarbeitungslebenszyklus
  (`pending → ready`, `202`-/`206`-Semantik), Beschreibungs-/Fokus-Updates,
  lokale Dateiauslieferung mit geschnifften Inhaltstypen.

**Timelines und Suche:**

- Home-, öffentliche (`local`/`remote`/`only_media`-), Hashtag-, Listen- und
  Konto-Timelines mit exklusiven numerischen Cursorn und `Link`-Headern.
- Suche ist `GET /api/v2/search` nur über den lokalen Korpus
  (Konten/Status/Hashtags) — keine Volltext-Engine.

**Benachrichtigungen, Marker, Listen, Filter:**

- Benachrichtigungs-Index/Anzeige/Löschen/Verwerfen/`unread_count`,
  `GET`+`POST /api/v1/markers`; vollständiges Listen-CRUD + Mitgliedschaft;
  Filter v1+v2 mit `filtered`-Annotation.
- Bereits drin aus Phase 7: Konto-Mehrabruf (`?id[]=`, bis zu 40), Lookup
  und Suche; `POST /api/v1/reports` (speichert + leitet `Flag` weiter, wenn
  gewünscht); Vorschläge und Ankündigungen antworten `[]`; Spendenkampagnen
  antworten `204`.

**Streaming:**

- In-Prozess-SSE und -WebSocket für `user`, `user:notification`, `public`
  (+`local`/`remote`-Medienvarianten), `hashtag` (+`local`) und `list`, mit
  Auth/Scopes, Heartbeats und expliziten Grenzen (512 gesamt, 16/Token,
  Fail-closed-Überlauf).
- Ereignisse: `update`, `delete`, `status.update`, `notification`.

**Föderation (ActivityPub):**

- WebFinger (`acct:`-Auflösung), NodeInfo 2.0, Akteur-/Schlüsseldokumente,
  gemeinsame + persönliche Inboxen, Outbox-/Follower-/Folge-Kollektionen,
  signierte Zustellung mit Wiederholung/Backoff/Quarantäne, SSRF-sicheres
  Abrufen (Größen-/Timeout-/Redirect-Limits, DNS-/IP-Schutz).
- Aktivitäten in beide Richtungen: Follow/Accept/Reject/Undo, Create/Update/Delete
  Note, Like/Undo, Announce/Undo (inkl. Undo bei Reblog-Löschung),
  Block/Undo, Frage-Umfragen + Stimmen, Zitat-Request/Accept/Reject/Revoke,
  Flag-Senden/-Empfangen, Abruf entfernter Medien (Originalbytes wortgetreu
  zwischengespeichert).
- Thread-Vervollständigung: ingest-seitige Elternauflösung (ein Hop pro Job,
  Hop-Limit + Abrufbudget, niemals zum Root hochstufen) plus authentifizierter
  Kontext-Crawl mit `Mastodon-Async-Refresh`.

**Haltbarkeit:**

- Eine `redb`-Transaktion committet Zustandsänderung + Indizes/Zähler +
  Idempotenz-/Dedup-Einträge + begrenzte haltbare Jobs atomar. Netzwerk-I/O,
  Medienverarbeitung, Benachrichtigungs-Fan-out und Zustellung geschehen nach
  dem Commit. Worker sind idempotent; Crash-Recovery-Tests beweisen, dass keine
  committete Änderung ihren Job verliert.

---

## Was es nicht kann

Bewusste Nicht-Ziele und aufgeschobene Arbeiten — wer danach fragt, erhält
`404` (oder das dokumentierte leere/204 unten) statt einer kaputten
Halbimplementierung:

- **Keine Unterhaltungs-/DM-Oberfläche.** Keine `GET /api/v1/conversations`-
  Familie, keine Unterhaltungs-Stummschaltungen. Direktnachrichten
  föderieren als Status, haben aber kein Threading und keine
  `direct`-Stream-Ereignisse.
- **Kein Web Push.** Keine Push-Abonnements oder -Zustellung (geplant Phase 7).
- **Keine Admin- & Moderations-API.** Kein `/api/v1/admin/*` oder
  `/api/v2/admin/*`, keine Rollen (CLI-Konten sind rollenlos), keine
  Meldewarteschlangen-UI, keine Trends, keine Jahresberichte (geplant
  Phase 7.3).
- **Keine Erstanbieter-Web-Client-Routen.** Kein `/api/web/*`, keine
  Keystodon-Bundle-Auslieferung (geplant Phase 7).
- **Keine Medientranskodierung.** Kein GIF→Video, kein VP9/WebM, kein WebP,
  kein `gifv` (geplant Phase 7.1).
- **Keine echte Suche.** Nur Scan des lokalen Korpus; keine eingebettete
  Volltext-Engine, kein Elasticsearch (geplant Phase 7.2).
- **Kein Mehrknotenbetrieb.** Nur Einzelknoten; kein
  Postgres-/Redis-/FoundationDB-Backend, keine Shared-Storage-Medienablage.
- **Noch nicht da:** E-Mail-Bestätigungsdurchsetzung, Einladungs-/
  Genehmigungs-Registrierungsmodi, Refresh-Tokens (`unsupported_grant_type`),
  `GET blocks/mutes/favourites/bookmarks`-Kollektionslesezugriffe,
  `notifications_merged`-Ereignis, Benachrichtigungs-`requests`-Pipeline (immer
  `[]`), Filterung nach gewählten Sprachen.

---

## Bekannte Lücken und Ecken

Dinge, die funktionieren, aber mit dokumentierten Grenzen, über die du
stolpern kannst:

- **Nur im Labor getestet.** Interop ist gegen echtes Mastodon 4.7.3 in einem
  privaten Docker-Bridge-Labor verifiziert, nie gegen das öffentliche Internet.
  Laborschalter (`--federation-allow-http`, `--federation-allow-private`,
  `--federation-extra-ca`) sind standardmäßig aus und dürfen nie in Produktion
  verwendet werden.
- **Medienaustausch mit Mastodon 4.7.x ist im Labor wackelig**
  (Abruf-/Re-Encode-Verhalten auf Mastodon-Seite; siehe
  `docs/federation-contract.md`). Noch keine Zustellabkühlung pro Ziel
  (Backoff ist pro Job). Ein Delete, das gegen ein Create rast, kann Reste
  hinterlassen (braucht Tombstones — das eine bekannte birchtree-verursachte
  Problem).
- **Medienabweichungen (akzeptiert):** animierte GIFs bleiben `image`, niemals
  `gifv` (D1); Video/Audio passieren unverändert durch (D2–D3); Standbilder
  werden nicht zu WebP konvertiert (D4). Phase 7.1 ändert den Packer (`type`,
  `url`, `preview_url`, `meta`, null-während-ausstehend).
- **Timeline-Abweichungen (akzeptiert):** Home liest nur die 1000 neuesten
  Follows (D10a); exklusive Listenmitglieder vor Home verborgen (D10b); kein
  `206`-Async-Refresh auf Home (D9).
- **Thread-Abweichung (D5):** nur-eingeloggt-Vorfahrenabruf beim
  Kontextlesen — Mastodon tut das nicht.
- **`direct`-Stream ist akzeptiert, aber stumm**, bis Unterhaltungen existieren
  (D18).
- **Einzelknoten-Speicherung.** Ein Schreiber (`redb`), keine Read-Replikas,
  kein Live-Migrationspfad zu Mehrknoten.
- **Minimale Beobachtbarkeit** (strukturierte Logs, `RUST_LOG`,
  nur Health/Readiness). Keine Zustellfeinabstimmungs-UI pro Ziel.

---

## Stand der Dinge

| Phase | Was es war | Status |
|---|---|---|
| 0 | Quellverträge einfrieren (Routeninventar vs. v4.7.3, DTOs, Sichtbarkeit, Ports) | In Prüfung — Routeninventar-Prüfung offen |
| 1 | Laufzeit, `redb`, IDs, Konfig, Bootstrap | Fertig |
| 2 | OAuth, Registrierung, exakte Lese-APIs | Fertig |
| 3 | Lokale Status, Beziehungen, Zitate, Idempotenz | Fertig |
| 4 | Medien, Timelines, Benachrichtigungen, Listen/Filter/Suche | Fertig |
| 4.1 | Paritätslückenschluss (JSON-Bodies D11, exklusive Listen D10b, Home-Cap D10a) | Fertig |
| 5 | Streaming (SSE + WebSocket, Envelopes, Grenzen) | Aktuell — weitgehend gebaut, Exit-Prüfung offen |
| 6 | ActivityPub-Föderation (Inboxen, Zustellung, Thread-Vervollständigung) | Kern gelandet + im Labor verifiziert; Härtung offen |
| 7 | Transkodierung (7.1), Suche (7.2), Admin/Moderation (7.3), Push, `/api/web/*` | Geplant — nur Mehrabruf/Meldungen/Stubs gelandet |
| 8 | Deklarierte API-Kompatibilität (98%-Paritätssuite) | Geplant |

Der Einzelknoten-Server ist **heute lokal nutzbar**. Deklarierte
v4.7.3-Kompatibilität wird erst zugesichert, wenn das Phase-8-Paritätsgate
bestanden ist.

---

## Eigenen Server betreiben

### Voraussetzungen

1. **Rust-Toolchain** (stable, Edition 2024) via [rustup](https://rustup.rs/).
2. **Eine C-Toolchain** (gcc/clang) für Krypto-Abhängigkeiten. Fedora:
   `sudo dnf install gcc`. Debian/Ubuntu: `sudo apt install build-essential`.
3. `curl`, um die API anzustoßen.

```bash
rustc --version   # 1.98+ recommended
cargo --version
```

### Bauen

```bash
git clone https://github.com/<you>/birchtree.git
cd birchtree
cargo build --release
```

Dies erzeugt ein einzelnes Binary unter `target/release/birchtree`
(`target/debug/birchtree` für `cargo build`).

### Starten

```bash
mkdir -p ~/.local/share/birchtree
./target/release/birchtree \
  --data-dir="$HOME/.local/share/birchtree" \
  --listen="0.0.0.0:3000" \
  --public-url="http://localhost:3000"
```

- `--public-url` ist die URL, die die Welt (und Clients) sieht. `http://` oder
  `https://` mit Host. Akteur-/Objekt-URIs, WebFinger und NodeInfo bauen
  darauf auf. `http://localhost:3000` reicht zum Basteln;
  `https://social.example.com` für eine echte Instanz.
- `--data-dir` hält die `redb`-Datei + hochgeladene Medien. Richte es auf ein
  Verzeichnis ein, das du sichern kannst. Standard `./data`.
- `--listen` ist die Bind-Adresse. `127.0.0.1:3000` hält ihn nur-lokal.

Prüfen, ob er lebt:

```bash
curl http://localhost:3000/health   # -> ok
curl http://localhost:3000/ready    # -> ready
```

Stoppen mit `Ctrl-C`. Für Produktion einen Reverse-Proxy (Caddy/nginx) für
HTTPS davorstellen — birchtree spricht schlichtes HTTP und vertraut
`X-Forwarded-For` nur von `--trusted-proxy=`-Adressen.

Vollständige Flag-Referenz (`--max-body-size`, `--max-status-chars`,
`--site-title/description/contact`, `--registration-open/closed`,
`--streaming-url`, `--federation-*`, `RUST_LOG`): siehe `--help` und die Tabelle
in der vorherigen README-Revision / `src/config.rs`.

### Erstes Konto anlegen

Registrierung ist **standardmäßig geschlossen**, also lege das erste Konto per
CLI an:

```bash
./target/release/birchtree create-account \
  --data-dir="$HOME/.local/share/birchtree" \
  --public-url="http://localhost:3000" \
  --username=alice \
  --password='a-strong-password' \
  --email='alice@example.com' \
  --display-name='Alice'
```

Es druckt `created account id=... username=alice`. Dieses Konto ist dein
Admin. Das Passwort kann stattdessen aus `BIRCHTREE_PASSWORD` statt
`--password=` kommen.

Um Anmeldungen zu öffnen:

```bash
./target/release/birchtree \
  --data-dir="$HOME/.local/share/birchtree" \
  --listen="0.0.0.0:3000" \
  --public-url="http://localhost:3000" \
  --registration-open
```

### Anmelden und posten

Richte einen beliebigen Mastodon-Client auf deine öffentliche URL ein oder
nutze die API direkt:

```bash
# 1. Register an OAuth app
curl -s -X POST http://localhost:3000/api/v1/apps \
  -d 'client_name=myclient' \
  -d 'redirect_uris=urn:ietf:wg:oauth:2.0:oob' \
  -d 'scopes=read write follow push'

# 2. Use client_id / client_secret in the authorization-code + PKCE flow,
#    then post with the user token:
curl -s -X POST http://localhost:3000/api/v1/statuses \
  -H 'Authorization: Bearer <token>' \
  -d 'status=hello from birchtree&visibility=public'
```

Instanzinfo: `GET /api/v1/instance` und `GET /api/v2/instance`.

---

## Tägliche Nutzung

Einmal laufend, interagiere wie mit jeder Mastodon-Instanz (formkodierte,
Multipart- oder JSON-Bodies; IDs sind Dezimalstrings; Cursor sind exklusive
numerische `max_id`/`since_id` mit `Link`-Headern):

- **Posten:** `POST /api/v1/statuses` mit `status`, `visibility`
  (`public`/`unlisted`/`private`/`direct`), `spoiler_text`, `in_reply_to_id`,
  `media_ids[]`, `poll[options][]`, `scheduled_at`, `quote`-Feldern. Bearbeiten
  via `PUT /api/v1/statuses/:id`, Löschen via `DELETE /api/v1/statuses/:id`.
- **Timelines:** `GET /api/v1/timelines/home|public`,
  `/api/v1/timelines/tag/:tag`, `/api/v1/timelines/list/:id`,
  `/api/v1/accounts/:id/statuses`.
- **Interagieren:**
  `/api/v1/statuses/:id/{favourite,unfavourite,reblog,unreblog,bookmark,unbookmark,pin,unpin}`,
  `/api/v1/polls/:id/votes`, `/api/v1/follows`,
  `/api/v1/blocks|mutes|domain_blocks`.
- **Medien:** `POST /api/v1/media` / `POST /api/v2/media` (multipart); Dateien
  werden lokal mit geschnifften Inhaltstypen ausgeliefert.
- **Benachrichtigungen/Marker:** `/api/v1/notifications`, `/api/v1/markers`.
- **Listen/Filter:** `/api/v1/lists`, `/api/v1/filters` (v1+v2).
- **Suche:** `GET /api/v2/search` (nur lokaler Korpus).
- **Streaming:** SSE oder WebSocket auf dem Streaming-Endpunkt mit
  OAuth-Token; Streams `user`, `user:notification`, `public`, `hashtag`,
  `list`.

Die volle Oberfläche steht in `docs/endpoint-manifest.md`. Alles außerhalb des
Profils antwortet `404` mit Mastodons `{ "error": "..." }`-Form (keine
Misskey-Hülle).

---

## Womit es spricht

birchtree föderiert über ActivityPub und wurde in einem privaten Labor
getestet — nie gegen das öffentliche Internet:

- **Mastodon 4.7.3** (gepinnter Commit `8ff11c2b`): Folgen, Posten, Antworten,
  Boosten, Liken, Löschen, Medien, Umfragen, Zitate bestehen in einer
  Fünf-Server-Bench (selbst, pinetree, Mastodon).
- **pinetree** (Misskey-Familie): Familien-übergreifende Bench besteht für den
  Kern-Aktivitätssatz.

Es spricht Create/Update/Delete Note, Follow/Accept/Reject/Undo, Like/Undo,
Announce/Undo, Block/Undo, Flag, Question + Stimmen und Zitat-
Request/Accept/Reject/Revoke, plus WebFinger, NodeInfo 2.0 und HTTP-
Signaturen. Föderationsinput wird als nicht vertrauenswürdig behandelt
(Größen-/Timeout-/Redirect-Limits, SSRF-Schutz).

---

## Projektstruktur

```
.
├── birchtree.md               product & architecture plan (source of truth, pins v4.7.3)
├── AGENTS.md                  rules for anyone (human or AI) working in this repo
├── docs/                      contracts (Status: Draft/Frozen) + wiki + logs/
│   ├── endpoint-manifest.md   the Mastodon API contract
│   ├── compatibility-profile.md  deviation register D1–D18
│   ├── federation-contract.md streaming-contract.md, visibility, ports, ...
│   └── wiki/                  living overview (About, Current-State, Roadmap, Feature-Matrix)
├── src/                       the Rust crate (one modular monolith)
│   ├── domain/                pure types & rules (no HTTP, no DB)
│   ├── application/           use cases (commands, queries, ports)
│   ├── api/mastodon/          HTTP adapter (router, DTOs, pagination, errors)
│   ├── activitypub/           protocol adapter (actor, objects, parser, inbox)
│   ├── federation/            resolver, delivery, retry, dedup, remote media
│   ├── persistence/redb/      embedded database adapter
│   ├── streaming/             hub, SSE, WebSocket
│   ├── jobs/                  durable job queue & worker
│   ├── media/                 ingest, blob store, transcode stub
│   └── config.rs              CLI flags (no config file)
├── tests/                     api_contract, oauth, persistence_conformance,
│                              crash_recovery, federation_interop, streaming
├── scripts/                   smoke-all.sh, bench-local.py, lab/
└── state/logs/                dated work logs with real command output
```

Die Abhängigkeitsrichtung ist `domain` ← `application` ← Adapter. REST und
ActivityPub rufen dieselben Anwendungsoperationen auf — keine doppelte
Geschäftslogik. Föderation selbst ist an `activitypub-federation-rust`
delegiert; `federation/` ist Richtlinie/Zustellung, keine eigene
Protokollimplementierung.

---

## Selbst testen

Das volle Verifikationsgate ist:

```bash
cargo check --all-targets      # must be warning-free
cargo test                     # full suite (currently 349 passed / 0 failed --release)
./scripts/smoke-all.sh         # check + tests + phase smoke scripts
```

Du kannst auch Teile einzeln laufen lassen (`cargo test <name>`). Siehe
`docs/testing.md` für die vollständige Anleitung.

Um Föderation gegen ein echtes Mastodon zu testen, siehe
`scripts/lab/README.md`. Es startet eine private Docker-Bridge mit Mastodon
4.7.3. Laborschalter sind standardmäßig aus und dürfen nie in Produktion
verwendet werden.

---

## Mitwirken & weiterlesen

- `birchtree.md` — der Produkt- und Architekturplan. Gilt bei jeder
  Abweichung von dieser README.
- `AGENTS.md` — die Spielregeln für Mitwirkende (Mensch oder KI).
- `docs/` — die Verträge (`endpoint-manifest.md`, `compatibility-profile.md`,
  `federation-contract.md`, `streaming-contract.md`, …) jeweils mit einer
  `Status:`-Zeile. Eingefrorene Dateien ändern sich nur via geloggter
  ablösender Entscheidung.
- `docs/wiki/` — lesbare Übersicht (About, Current state, Roadmap).
- `state/logs/` + `docs/logs/` — datierte Arbeitslogs und
  Verifikationsbelege mit echter Ausgabe. Lies den neuesten Eintrag, um auf
  Stand zu kommen.

Wer helfen will: am nützlichsten sind Ausprobieren gegen einen
Mastodon-kompatiblen Client, Laufenlassen der Testsuite und des
Föderationslabors sowie Issue-Meldungen. Transkodierung (7.1), Suche (7.2)
und Admin/Moderation (7.3) sind die offenen Fronten.

---

## Lizenz

AGPL-3.0 — siehe `LICENSE`.
