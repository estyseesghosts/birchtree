# birchtree

> ce projet évolue vite, et ce document peut rapidement devenir obsolète. Le README anglais à la racine du projet est généralement le plus à jour.

birchtree est un **serveur fediverse compatible avec l'API Mastodon**,
monoprocessus, écrit en Rust. Il réimplémente [Mastodon **4.7.3**](https://github.com/mastodon/mastodon/blob/v4.7.3/CHANGELOG.md)
(API v7) adossé à `redb` embarqué et à des médias sur disque local. La fédération parle
ActivityPub via
[`activitypub-federation-rust`](https://github.com/LemmyNet/activitypub-federation-rust)
avec en plus les règles de politique et de distribution propres à birchtree.

Il est volontairement **léger à exploiter** : ni Postgres, ni Redis, ni bus de messages.
Un seul binaire, un seul répertoire de données (fichier `redb` + médias), et c'est tout.

birchtree ne fournit pas d'interface web. Pointez n'importe quel client compatible Mastodon
vers lui — voir [keystone](https://github.com/estyseesghosts/keystone) (en
développement). L'interface web officielle de Mastodon n'a pas encore été testée contre lui.
Les résultats peuvent varier.

> **Statut (2026-10-20) :** utilisable en local et fédérant en laboratoire
> (`cargo test --release` : 349 réussis / 0 échoués). Les comptes, les statuts,
> les relations, les médias, les fils, les notifications/marqueurs/listes/filtres,
> le streaming (SSE + WebSocket) et le cœur de la fédération sont implémentés et
> vérifiés en laboratoire contre un vrai Mastodon 4.7.3. Les conversations, le Web Push, l'API
> d'administration, `/api/web/*`, le transcodage, la recherche plein texte et le multinœud sont
> planifiés, pas construits. Testé uniquement dans des environnements simulés et un labo privé
> — jamais contre le fediverse public.

---

## Table des matières

- [Ce qu'il fait aujourd'hui](#what-it-does-today)
- [Ce qu'il ne fait pas](#what-it-does-not-do)
- [Lacunes connues et aspérités](#known-gaps-and-rough-edges)
- [État d'avancement](#where-things-stand)
- [Faire tourner votre propre serveur](#run-your-own-server)
  - [Prérequis](#prerequisites)
  - [Le construire](#build-it)
  - [Le démarrer](#start-it)
  - [Créer votre premier compte](#create-your-first-account)
  - [Se connecter et publier](#log-in-and-post)
- [Utilisation au quotidien](#use-it-day-to-day)
- [Avec quoi il communique](#what-it-talks-to)
- [Organisation du projet](#project-layout)
- [Le tester vous-même](#testing-it-yourself)
- [Contribuer et aller plus loin](#contributing--reading-more)
- [Licence](#license)

---

## Ce qu'il fait aujourd'hui

Tout dans cette section tourne sur le serveur mononœud et est couvert par
des tests exécutés. Les routes non implémentées renvoient un `404` propre — jamais de fausses données —
avec les exceptions notées ci-dessous (points d'accès qui répondent correctement vide
parce que leur source de données n'existe pas encore). Le contrat exact est
`docs/endpoint-manifest.md` plus `docs/compatibility-profile.md` (registre des écarts D1–D18).

**Comptes, auth et inscription :**

- Enregistrement d'application OAuth2 (`POST /api/v1/apps`), flux authorization-code avec
  PKCE S256, applications client-credentials, révocation de jeton, application des scopes par route,
  pages de connexion/consentement rendues côté serveur.
- Politique d'inscription (ouverte/fermée), compte bootstrap via la CLI. L'envoi d'e-mails est un
  stub de journalisation — aucune vérification.
- IDs décimaux compatibles Mastodon : IDs 64 bits orientés horodatage sérialisés comme
  chaînes décimales ; comparaison numérique en interne, jamais lexicographique.

**Statuts et interactions :**

- Publication, affichage, modification (avec historique), suppression, réponses + contexte, boosts,
  favoris, signets, épingles (limite de 5), `favourited_by`/`reblogged_by`,
  vue source, récupération multiple, rejeu par `Idempotency-Key` (rétention d'une heure).
- Citations comme machine à états locale : politique (`public`/`followers`/`nobody`),
  acceptation/révocation, point d'accès de politique d'interaction, `quotes_count`.
- Sondages locaux (création/vote/expiration, totaux masqués) et statuts planifiés
  (avec parité 422 pour `<5 min`).
- La limite de texte des statuts vaut 500 caractères par défaut (parité Mastodon), configurable jusqu'à
  3000 via `--max-status-chars=`.

**Relations :**

- Suivi (comptes verrouillés → demandes en attente avec acceptation/rejet),
  désabonnement, retrait d'un abonné, blocage, mise en sourdine (avec expiration), blocages de domaines
  (forme amont v4.7.3), mises en avant, notes sur les comptes, listes abonnés/abonnements,
  agrégats de relations par lecteur.

**Médias :**

- Téléversement multipart v1/v2 avec détection MIME, cycle de vie de traitement
  (`pending → ready`, sémantique `202`/`206`), mises à jour description/focus,
  service des fichiers locaux avec types de contenu détectés.

**Fils et recherche :**

- Fils d'accueil, public (`local`/`remote`/`only_media`), hashtag, liste et compte
  avec curseurs numériques exclusifs et en-têtes `Link`.
- La recherche est `GET /api/v2/search` sur le corpus local uniquement
  (comptes/statuts/hashtags) — pas de moteur plein texte.

**Notifications, marqueurs, listes, filtres :**

- Index/affichage/effacement/rejet des notifications, `unread_count`,
  `GET`+`POST /api/v1/markers` ; CRUD complet des listes + appartenances ; filtres v1+v2
  avec annotation `filtered`.
- Déjà là depuis la phase 7 : récupération multiple de comptes (`?id[]=`, jusqu'à 40), résolution
  et recherche ; `POST /api/v1/reports` (stocke + relaie `Flag` sur demande) ;
  les suggestions et les annonces répondent `[]` ; les campagnes de dons répondent `204`.

**Streaming :**

- SSE et WebSocket en processus pour `user`, `user:notification`, `public`
  (+variantes `local`/`remote`/média), `hashtag` (+`local`) et `list`, avec
  auth/scopes, pulsations et bornes explicites (512 au total, 16/jeton,
  dépassement fail-closed).
- Événements : `update`, `delete`, `status.update`, `notification`.

**Fédération (ActivityPub) :**

- WebFinger (résolution `acct:`), NodeInfo 2.0, documents acteur/clé, boîtes partagées +
  personnelles, collections boîte d'envoi/abonnés/abonnements, distribution signée
  avec réessais/backoff/quarantaine, récupération protégée contre le SSRF (limites de taille/délai/redirections,
  défenses DNS/IP).
- Activités dans les deux sens : Follow/Accept/Reject/Undo, Create/Update/Delete
  Note, Like/Undo, Announce/Undo (y compris Undo sur suppression de boost),
  Block/Undo, sondages Question + votes, citation Request/Accept/Reject/Revoke,
  envoi/réception de Flag, récupération des médias distants (octets d'origine mis en cache tels quels).
- Complétion des fils : résolution des parents à l'ingestion (un saut par tâche, plafond de sauts
  + budget de récupération, jamais de promotion à la racine) plus exploration de contexte authentifiée
  avec `Mastodon-Async-Refresh`.

**Durabilité :**

- Une seule transaction `redb` valide mutation d'état + index/compteurs +
  enregistrements d'idempotence/déduplication + tâches durables bornées de façon atomique. Les E/S réseau,
  le traitement des médias, la diffusion des notifications et la distribution ont lieu après validation.
  Les travailleurs sont idempotents ; les tests de reprise sur panne prouvent qu'aucune mutation validée
  ne perd sa tâche.

---

## Ce qu'il ne fait pas

Non-objectifs délibérés et travaux reportés — demander cela renvoie `404`
(ou le vide/`204` documenté ci-dessous) plutôt qu'une implémentation bancale à moitié finie :

- **Pas de surface conversations / DM.** Pas de famille `GET /api/v1/conversations`,
  pas de mise en sourdine de conversations. Les messages directs fédèrent comme des statuts mais
  n'ont ni fils ni événements de flux `direct`.
- **Pas de Web Push.** Ni abonnements push ni distribution (phase 7 planifiée).
- **Pas d'API d'administration ni de modération.** Ni `/api/v1/admin/*` ni `/api/v2/admin/*`,
  ni rôles (les comptes CLI sont sans rôle), ni file de signalements, ni tendances, ni
  rapports annuels (phase 7.3 planifiée).
- **Pas de routes de client web maison.** Ni `/api/web/*`, ni service du paquet keystodon
  (phase 7 planifiée).
- **Pas de transcodage média.** Ni GIF→vidéo, ni VP9/WebM, ni WebP, ni `gifv`
  (phase 7.1 planifiée).
- **Pas de vraie recherche.** Balayage du corpus local uniquement ; ni moteur plein texte embarqué,
  ni Elasticsearch (phase 7.2 planifiée).
- **Pas de mode multinœud.** Mononœud uniquement ; ni moteur Postgres/Redis/FoundationDB,
  ni arrangement médias en stockage partagé.
- **Pas encore là :** vérification de l'e-mail, modes d'inscription sur invitation/approbation,
  jetons de rafraîchissement (`unsupported_grant_type`), lectures de collections `GET
  blocks/mutes/favourites/bookmarks`, événement `notifications_merged`,
  pipeline de `requests` de notifications (toujours `[]`), filtrage par langues choisies.

---

## Lacunes connues et aspérités

Des choses qui marchent mais avec des limites documentées sur lesquelles vous pourriez buter :

- **Testé en labo uniquement.** L'interopérabilité est vérifiée contre un vrai Mastodon 4.7.3 dans un
  labo privé sur pont Docker, jamais contre l'internet public. Les commutateurs de labo
  (`--federation-allow-http`, `--federation-allow-private`,
  `--federation-extra-ca`) sont désactivés par défaut et ne doivent jamais être utilisés en
  production.
- **L'échange de médias avec Mastodon 4.7.x est instable en labo**
  (comportement de récupération/réencodage côté Mastodon ; voir
  `docs/federation-contract.md`). Pas encore de délai entre tentatives par destination
  (le backoff est par tâche). Un Delete qui devance un Create peut laisser un résidu
  (il faut des pierres tombales — le seul problème connu causé par birchtree).
- **Écarts médias (acceptés) :** les GIF animés restent `image`, jamais `gifv`
  (D1) ; vidéo/audio transmis tels quels (D2–D3) ; images fixes non converties en
  WebP (D4). La phase 7.1 change l'emballeur (`type`, `url`, `preview_url`,
  `meta`, nul-en-attente).
- **Écarts de fils (acceptés) :** l'accueil ne lit que les 1000 abonnements les plus récents
  (D10a) ; membres des listes exclusives masqués dans l'accueil (D10b) ; pas de `206`
  de rafraîchissement asynchrone sur l'accueil (D9).
- **Écart de fils (D5) :** récupération des ancêtres réservée aux connectés à la lecture du contexte —
  Mastodon ne fait pas cela.
- **Le flux `direct` est accepté mais silencieux** tant que les conversations n'existent pas (D18).
- **Stockage mononœud.** Un seul écrivain (`redb`), pas de réplicas de lecture, pas de chemin de
  migration à chaud vers le multinœud.
- **Observabilité minimale** (journaux structurés, `RUST_LOG`, santé/préparation
  uniquement). Pas d'interface de réglage de la distribution par destination.

---

## État d'avancement

| Phase | De quoi il s'agissait | Statut |
|---|---|---|
| 0 | Figer les contrats sources (inventaire des routes face à v4.7.3, DTO, visibilité, ports) | En révision — revue de l'inventaire des routes ouverte |
| 1 | Runtime, `redb`, IDs, configuration, bootstrap | Terminé |
| 2 | OAuth, inscription, API de lecture exactes | Terminé |
| 3 | Statuts locaux, relations, citations, idempotence | Terminé |
| 4 | Médias, fils, notifications, listes/filtres/recherche | Terminé |
| 4.1 | Résorption des écarts de parité (corps JSON D11, listes exclusives D10b, plafond d'accueil D10a) | Terminé |
| 5 | Streaming (SSE + WebSocket, enveloppes, bornes) | En cours — largement construit, revue de sortie ouverte |
| 6 | Fédération ActivityPub (boîtes de réception, distribution, complétion des fils) | Cœur intégré + vérifié en labo ; durcissement en cours |
| 7 | Transcodage (7.1), recherche (7.2), administration/modération (7.3), push, `/api/web/*` | Planifié — seuls multi-fetch/signalements/stubs intégrés |
| 8 | Compatibilité d'API déclarée (suite de parité à 98 %) | Planifié |

Le serveur mononœud est **utilisable en local dès aujourd'hui**. La compatibilité
déclarée avec v4.7.3 n'est pas revendiquée tant que le palier de parité de la phase 8 n'est pas franchi.

---

## Faire tourner votre propre serveur

### Prérequis

1. **Chaîne d'outils Rust** (stable, édition 2024) via [rustup](https://rustup.rs/).
2. **Une chaîne de compilation C** (gcc/clang) pour les dépendances crypto. Fedora :
   `sudo dnf install gcc`. Debian/Ubuntu : `sudo apt install build-essential`.
3. `curl` pour interroger l'API.

```bash
rustc --version   # 1.98+ recommended
cargo --version
```

### Le construire

```bash
git clone https://github.com/<you>/birchtree.git
cd birchtree
cargo build --release
```

Cela produit un binaire unique dans `target/release/birchtree`
(`target/debug/birchtree` pour `cargo build`).

### Le démarrer

```bash
mkdir -p ~/.local/share/birchtree
./target/release/birchtree \
  --data-dir="$HOME/.local/share/birchtree" \
  --listen="0.0.0.0:3000" \
  --public-url="http://localhost:3000"
```

- `--public-url` est l'URL que le monde (et les clients) voit. `http://` ou
  `https://` avec un hôte. Les URI d'acteurs/objets, WebFinger et NodeInfo sont construits
  à partir d'elle. `http://localhost:3000` convient pour bricoler ;
  `https://social.example.com` pour une vraie instance.
- `--data-dir` contient le fichier `redb` + les médias téléversés. Pointez-le vers un
  répertoire que vous pouvez sauvegarder. Défaut `./data`.
- `--listen` est l'adresse d'écoute. `127.0.0.1:3000` le garde local uniquement.

Vérifiez qu'il est en vie :

```bash
curl http://localhost:3000/health   # -> ok
curl http://localhost:3000/ready    # -> ready
```

Arrêtez avec `Ctrl-C`. En production, placez un proxy inverse (Caddy/nginx) devant
pour HTTPS — birchtree parle HTTP simple et ne fait confiance à `X-Forwarded-For`
que depuis les adresses `--trusted-proxy=`.

Référence complète des options (`--max-body-size`, `--max-status-chars`,
`--site-title/description/contact`, `--registration-open/closed`,
`--streaming-url`, `--federation-*`, `RUST_LOG`) : voir `--help` et le tableau
dans la révision précédente du README / `src/config.rs`.

### Créer votre premier compte

L'inscription est **fermée par défaut**, alors initialisez le premier compte depuis
la CLI :

```bash
./target/release/birchtree create-account \
  --data-dir="$HOME/.local/share/birchtree" \
  --public-url="http://localhost:3000" \
  --username=alice \
  --password='a-strong-password' \
  --email='alice@example.com' \
  --display-name='Alice'
```

Il affiche `created account id=... username=alice`. Ce compte est votre
administrateur. Le mot de passe peut venir de `BIRCHTREE_PASSWORD` au lieu de `--password=`.

Pour ouvrir les inscriptions :

```bash
./target/release/birchtree \
  --data-dir="$HOME/.local/share/birchtree" \
  --listen="0.0.0.0:3000" \
  --public-url="http://localhost:3000" \
  --registration-open
```

### Se connecter et publier

Pointez n'importe quel client Mastodon vers votre URL publique, ou utilisez l'API directement :

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

Infos d'instance : `GET /api/v1/instance` et `GET /api/v2/instance`.

---

## Utilisation au quotidien

Une fois lancé, interagissez comme avec n'importe quelle instance Mastodon (corps en formulaire, multipart
ou JSON ; IDs en chaînes décimales ; curseurs numériques exclusifs
`max_id`/`since_id` avec en-têtes `Link`) :

- **Publier :** `POST /api/v1/statuses` avec `status`, `visibility`
  (`public`/`unlisted`/`private`/`direct`), `spoiler_text`, `in_reply_to_id`,
  `media_ids[]`, `poll[options][]`, `scheduled_at`, champs `quote`. Modification via
  `PUT /api/v1/statuses/:id`, suppression via `DELETE /api/v1/statuses/:id`.
- **Fils :** `GET /api/v1/timelines/home|public`, `/api/v1/timelines/tag/:tag`,
  `/api/v1/timelines/list/:id`, `/api/v1/accounts/:id/statuses`.
- **Interagir :** `/api/v1/statuses/:id/{favourite,unfavourite,reblog,unreblog,bookmark,unbookmark,pin,unpin}`,
  `/api/v1/polls/:id/votes`, `/api/v1/follows`, `/api/v1/blocks|mutes|domain_blocks`.
- **Médias :** `POST /api/v1/media` / `POST /api/v2/media` (multipart) ; fichiers
  servis localement avec types de contenu détectés.
- **Notifications/marqueurs :** `/api/v1/notifications`, `/api/v1/markers`.
- **Listes/filtres :** `/api/v1/lists`, `/api/v1/filters` (v1+v2).
- **Recherche :** `GET /api/v2/search` (corpus local uniquement).
- **Streaming :** SSE ou WebSocket sur le point d'accès de streaming avec jeton OAuth ;
  flux `user`, `user:notification`, `public`, `hashtag`, `list`.

La surface complète est dans `docs/endpoint-manifest.md`. Tout ce qui est hors profil
renvoie `404` avec la forme `{ "error": "..." }` de Mastodon (pas une enveloppe Misskey).

---

## Avec quoi il communique

birchtree fédère via ActivityPub et a été testé dans un labo privé —
jamais contre l'internet public :

- **Mastodon 4.7.3** (commit épinglé `8ff11c2b`) : suivis, publications, réponses,
  boosts, favoris, suppressions, médias, sondages, citations passent dans un banc à cinq serveurs
  (soi-même, pinetree, Mastodon).
- **pinetree** (famille Misskey) : le banc interfamilles passe pour le jeu d'activités central.

Il parle Create/Update/Delete Note, Follow/Accept/Reject/Undo, Like/Undo,
Announce/Undo, Block/Undo, Flag, Question + votes, et citation
Request/Accept/Reject/Revoke, plus WebFinger, NodeInfo 2.0 et signatures HTTP.
Les entrées de fédération sont traitées comme non fiables (limites de taille/délai/redirections,
protection SSRF).

---

## Organisation du projet

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

Le sens des dépendances est `domain` ← `application` ← adaptateurs. REST et
ActivityPub appellent les mêmes opérations applicatives — pas de logique métier dupliquée.
La fédération elle-même est déléguée à `activitypub-federation-rust` ;
`federation/` est politique/distribution, pas une implémentation de protocole maison.

---

## Le tester vous-même

La passe complète de vérification est la suivante :

```bash
cargo check --all-targets      # must be warning-free
cargo test                     # full suite (currently 349 passed / 0 failed --release)
./scripts/smoke-all.sh         # check + tests + phase smoke scripts
```

Vous pouvez aussi lancer des morceaux individuellement (`cargo test <name>`). Voir
`docs/testing.md` pour le guide complet.

Pour tester la fédération contre un vrai Mastodon, voir `scripts/lab/README.md`. Il
démarre un pont Docker privé avec Mastodon 4.7.3. Les commutateurs de labo sont désactivés
par défaut et ne doivent jamais être utilisés en production.

---

## Contribuer et aller plus loin

- `birchtree.md` — le plan produit et architecture. Prime sur tout
  désaccord avec ce README.
- `AGENTS.md` — les règles du jeu pour les contributeurs (humains ou IA).
- `docs/` — les contrats (`endpoint-manifest.md`, `compatibility-profile.md`,
  `federation-contract.md`, `streaming-contract.md`, …) chacun avec une
  ligne `Status:`. Les fichiers figés ne changent que via une décision enregistrée qui les remplace.
- `docs/wiki/` — présentation lisible (About, état actuel, feuille de route).
- `state/logs/` + `docs/logs/` — journaux de travail datés et preuves de vérification
  avec vraies sorties. Lisez la dernière entrée pour vous mettre à niveau.

Si vous voulez aider, le plus utile est : l'essayer contre un
client compatible Mastodon, lancer la suite de tests et le labo de fédération,
et signaler les problèmes. Le transcodage (7.1), la recherche (7.2) et l'administration/modération
(7.3) sont les frontières ouvertes.

---

## Licence

AGPL-3.0 — voir `LICENSE`.
