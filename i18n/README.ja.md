# birchtree

> このプロジェクトは変化が速く、この文書はすぐに古くなる可能性があります。プロジェクトルートにある英語の README が通常は最新です。

birchtree は、Rust で書かれたシングルプロセスの **Mastodon API 互換 fediverse サーバー**です。[Mastodon **4.7.3**](https://github.com/mastodon/mastodon/blob/v4.7.3/CHANGELOG.md)（API v7）を再実装したもので、組み込みの `redb` とローカルディスク上のメディアで動作します。連合は
[`activitypub-federation-rust`](https://github.com/LemmyNet/activitypub-federation-rust)
に birchtree 独自のポリシーと配送ルールを組み合わせて ActivityPub を扱います。

**小さく運用できる**ことを意図しています。Postgres も Redis もメッセージブローカーも不要です。1つのバイナリと1つのデータディレクトリ（`redb` ファイル＋メディア）で完結します。

birchtree にフロントエンドは付属しません。Mastodon 互換の任意のクライアントを向けてください — [keystone](https://github.com/estyseesghosts/keystone)（開発中）を参照してください。公式 Mastodon フロントエンドとの組み合わせはまだテストしていません。動作は保証外です。

> **状態（2026-10-20）：** ローカルで利用可能、ラボ環境で連合も動作中
> （`cargo test --release`：349 件成功／0 件失敗）。アカウント、ステータス、
> リレーションシップ、メディア、タイムライン、通知／マーカー／リスト／フィルター、
> ストリーミング（SSE＋WebSocket）、コア連合は実装済みで、
> 実際の Mastodon 4.7.3 に対してラボ検証済みです。会話、Web Push、管理
> API、`/api/web/*`、トランスコード、全文検索、マルチノードは
> 計画中で未実装です。シミュレーション環境とプライベートなラボでのみ
> テストしています — 公開 fediverse に対しては一度もテストしていません。

---

## 目次

- [今日できること](#what-it-does-today)
- [できないこと](#what-it-does-not-do)
- [既知の不足と粗い部分](#known-gaps-and-rough-edges)
- [現在の状況](#where-things-stand)
- [自分のサーバーを動かす](#run-your-own-server)
  - [前提条件](#prerequisites)
  - [ビルド](#build-it)
  - [起動](#start-it)
  - [最初のアカウントを作る](#create-your-first-account)
  - [ログインして投稿する](#log-in-and-post)
- [日常の使い方](#use-it-day-to-day)
- [連携相手](#what-it-talks-to)
- [プロジェクト構成](#project-layout)
- [自分でテストする](#testing-it-yourself)
- [貢献と関連ドキュメント](#contributing--reading-more)
- [ライセンス](#license)

---

## 今日できること

この節の内容はすべてシングルノードサーバー上で動作し、実行済みテストでカバーされています。未実装のルートはきれいな `404` を返します — 偽のデータは決して返しません — ただし以下で注記した例外（データソースがまだ存在しないため正しく空を答えるエンドポイント）を除きます。正確な契約は `docs/endpoint-manifest.md` と `docs/compatibility-profile.md`（差分登録 D1–D18）です。

**アカウント、認証、登録：**

- OAuth2 アプリ登録（`POST /api/v1/apps`）、PKCE S256 付き認可コードフロー、クライアントクレデンシャルアプリ、トークン失効、ルートごとのスコープ適用、サーバー描画のログイン／同意ページ。
- 登録ポリシー（開放／閉鎖）、CLI ブートストラップアカウント。メール送信はログ出力のスタブです — 適用は行いません。
- Mastodon 互換の decimal ID：64 ビットのタイムスタンプ指向 ID を decimal 文字列としてシリアライズします。内部比較は数値で行い、辞書式順序は使いません。

**ステータスとインタラクション：**

- 投稿、表示、編集（履歴付き）、削除、返信＋コンテキスト、リブログ、お気に入り、ブックマーク、ピン（上限 5 件）、`favourited_by`／`reblogged_by`、ソース表示、複数取得、`Idempotency-Key` リプレイ（1 時間保持）。
- 引用はローカル状態機械として実装：ポリシー（`public`／`followers`／`nobody`）、承認／取消、インタラクションポリシーエンドポイント、`quotes_count`。
- ローカル投票（作成／投票／期限切れ、合計非表示）と予約ステータス（`<5 min` 422 パリティ付き）。
- ステータス本文の上限は既定で 500 文字（Mastodon パリティ）、`--max-status-chars=` で 3000 まで設定可能です。

**リレーションシップ：**

- フォロー（鍵アカウント → 承認／拒否付きの保留リクエスト）、フォロー解除、フォロワーからの削除、ブロック、ミュート（期限付き）、ドメインブロック（上流 v4.7.3 の形状）、承認（endorse）、アカウントメモ、フォロワー／フォロー中リスト、閲覧者ごとのリレーションシップ集計。

**メディア：**

- v1／v2 マルチパートアップロード（MIME スニッフィング付き）、処理ライフサイクル（`pending → ready`、`202`／`206` セマンティクス）、説明文／フォーカス更新、スニッフィングしたコンテントタイプでのローカルファイル配信。

**タイムラインと検索：**

- ホーム、パブリック（`local`／`remote`／`only_media`）、ハッシュタグ、リスト、アカウントタイムライン。排他的数値カーソルと `Link` ヘッダー付き。
- 検索はローカルコーパスに対する `GET /api/v2/search` のみ（アカウント／ステータス／ハッシュタグ） — 全文検索エンジンはありません。

**通知、マーカー、リスト、フィルター：**

- 通知の一覧／表示／クリア／破棄／`unread_count`、`GET`＋`POST /api/v1/markers`。リストの完全な CRUD＋メンバー管理。`filtered` 注釈付きのフィルター v1＋v2。
- Phase 7 から既に入っているもの：アカウント複数取得（`?id[]=`、最大 40 件）、lookup と search。`POST /api/v1/reports`（要求されれば `Flag` を保存＋転送）。おすすめとお知らせは `[]` を返します。寄付キャンペーンは `204` を返します。

**ストリーミング：**

- `user`、`user:notification`、`public`（＋`local`／`remote`／メディア変種）、`hashtag`（＋`local`）、`list` 向けのプロセス内 SSE と WebSocket。認証／スコープ、ハートビート、明示的な上限付き（合計 512、トークンあたり 16、上限超過は fail-closed）。
- イベント：`update`、`delete`、`status.update`、`notification`。

**連合（ActivityPub）：**

- WebFinger（`acct:` 解決）、NodeInfo 2.0、actor／鍵ドキュメント、共有＋個人 inbox、outbox／followers／following コレクション、リトライ／バックオフ／隔離付きの署名配送、SSRF 安全なフェッチ（サイズ／タイムアウト／リダイレクト制限、DNS／IP 防御）。
- 双方向のアクティビティ：Follow／Accept／Reject／Undo、Note の Create／Update／Delete、Like／Undo、Announce／Undo（リブログ削除時の Undo を含む）、Block／Undo、投票 Question＋投票、引用 Request／Accept／Reject／Revoke、Flag 送受信、リモートメディア取得（元バイト列をそのままキャッシュ）。
- スレッド補完：取り込み時の親解決（ジョブあたり 1 ホップ、ホップ上限＋取得予算、ルートへの昇格なし）と、`Mastodon-Async-Refresh` 付きの認証済みコンテキストクロール。

**耐久性：**

- 1 回の `redb` トランザクションで、状態変更＋インデックス／カウンタ＋冪等／重複排除レコード＋上限付き永続ジョブを原子的にコミットします。ネットワーク I/O、メディア処理、通知ファンアウト、配送はコミット後に行います。ワーカーは冪等であり、クラッシュ回復テストにより、コミット済みの変更がジョブを失わないことが証明されています。

---

## できないこと

意図的な非目標と延期した作業 — これらを求めると、壊れた半実装ではなく `404`（または下記の文書化された空／204）を返します：

- **会話／DM の UI 領域なし。** `GET /api/v1/conversations` 系なし、会話ミュートなし。ダイレクトメッセージはステータスとして連合しますが、スレッド化も `direct` ストリームイベントもありません。
- **Web Push なし。** プッシュ購読も配送もありません（Phase 7 で計画）。
- **管理＆モデレーション API なし。** `/api/v1/admin/*` も `/api/v2/admin/*` もなし、ロールなし（CLI アカウントはロールを持ちません）、通報キューの UI なし、トレンドなし、年次レポートなし（Phase 7.3 で計画）。
- **自前のウェブクライアントルートなし。** `/api/web/*` なし、keystodon バンドルの配信なし（Phase 7 で計画）。
- **メディアのトランスコードなし。** GIF→動画なし、VP9／WebM なし、WebP なし、`gifv` なし（Phase 7.1 で計画）。
- **真の検索なし。** ローカルコーパスの走査のみ。組み込み全文検索エンジンなし、Elasticsearch なし（Phase 7.2 で計画）。
- **マルチノードモードなし。** シングルノードのみ。Postgres／Redis／FoundationDB バックエンドなし、共有ストレージメディア構成なし。
- **まだ未対応：** メール確認の適用、招待／承認制の登録モード、リフレッシュトークン（`unsupported_grant_type`）、`GET blocks/mutes/favourites/bookmarks` コレクション読み取り、`notifications_merged` イベント、通知 `requests` パイプライン（常に `[]`）、選択言語フィルタリング。

---

## 既知の不足と粗い部分

動作はするものの、文書化された制限でつまずく可能性があるものです：

- **ラボテストのみ。** 相互運用性はプライベートな Docker ブリッジラボ内の実際の Mastodon 4.7.3 に対して検証されており、公開インターネットに対しては一度も検証していません。ラボ用スイッチ（`--federation-allow-http`、`--federation-allow-private`、`--federation-extra-ca`）は既定で無効であり、本番では決して使ってはいけません。
- **Mastodon 4.7.x とのメディア交換はラボで不安定です**（Mastodon 側の取得／再エンコード動作によるものです。`docs/federation-contract.md` を参照）。宛先ごとの配送クールダウンはまだなく（バックオフはジョブごとです）。Create と競合した Delete が残留物を残す場合があります（tombstone が必要 — birchtree 起因の既知の唯一の問題です）。
- **メディアの差分（許容済み）：** アニメーション GIF は `image` のままで `gifv` にはなりません（D1）。動画／音声はそのまま通過します（D2–D3）。静止画は WebP に変換されません（D4）。Phase 7.1 でパッカーが変わります（`type`、`url`、`preview_url`、`meta`、保留中の null）。
- **タイムラインの差分（許容済み）：** ホームは直近 1000 フォローのみを読みます（D10a）。排他リストのメンバーはホームから隠されます（D10b）。ホームに `206` 非同期リフレッシュはありません（D9）。
- **スレッドの差分（D5）：** コンテキスト読み取り時のログイン済みのみ祖先取得 — Mastodon はこれを行いません。
- **`direct` ストリームは許容済みですが無音です**（会話ができるまで）（D18）。
- **シングルノードストレージ。** 単一ライター（`redb`）、読み取りレプリカなし、マルチノードへのライブ移行パスなし。
- **最小限の可観測性**（構造化ログ、`RUST_LOG`、ヘルス／レディネスのみ）。宛先ごとの配送調整 UI なし。

---

## 現在の状況

| フェーズ | 内容 | 状態 |
|---|---|---|
| 0 | ソース契約の固定（v4.7.3 とのルートインベントリ比較、DTO、可視性、ポート） | レビュー中 — ルートインベントリのレビューが未了 |
| 1 | ランタイム、`redb`、ID、設定、ブートストラップ | 完了 |
| 2 | OAuth、登録、厳密な読み取り API | 完了 |
| 3 | ローカルステータス、リレーションシップ、引用、冪等性 | 完了 |
| 4 | メディア、タイムライン、通知、リスト／フィルター／検索 | 完了 |
| 4.1 | パリティギャップ解消（JSON ボディ D11、排他リスト D10b、ホーム上限 D10a） | 完了 |
| 5 | ストリーミング（SSE＋WebSocket、エンベロープ、上限） | 現在 — ほぼ実装済み、終了レビューが未了 |
| 6 | ActivityPub 連合（inbox、配送、スレッド補完） | コア実装済み＋ラボ検証済み、堅牢化が未了 |
| 7 | トランスコード（7.1）、検索（7.2）、管理／モデレーション（7.3）、プッシュ、`/api/web/*` | 計画中 — 複数取得／通報／スタブのみ実装済み |
| 8 | 宣言 API 互換性（98% パリティスイート） | 計画中 |

シングルノードサーバーは**今日ローカルで利用可能**です。Phase 8 のパリティゲートを通過するまで、宣言された v4.7.3 互換性は主張しません。

---

## 自分のサーバーを動かす

### 前提条件

1. **Rust ツールチェイン**（stable、edition 2024）を [rustup](https://rustup.rs/) 経由で。
2. 暗号依存のための **C ツールチェイン**（gcc／clang）。Fedora の場合：`sudo dnf install gcc`。Debian／Ubuntu の場合：`sudo apt install build-essential`。
3. API を叩くための `curl`。

```bash
rustc --version   # 1.98+ recommended
cargo --version
```

### ビルド

```bash
git clone https://github.com/<you>/birchtree.git
cd birchtree
cargo build --release
```

これにより単一バイナリが `target/release/birchtree` に生成されます（`cargo build` の場合は `target/debug/birchtree`）。

### 起動

```bash
mkdir -p ~/.local/share/birchtree
./target/release/birchtree \
  --data-dir="$HOME/.local/share/birchtree" \
  --listen="0.0.0.0:3000" \
  --public-url="http://localhost:3000"
```

- `--public-url` は世界（とクライアント）から見える URL です。ホスト付きの `http://` または `https://` を指定します。Actor／オブジェクト URI、WebFinger、NodeInfo はここから組み立てられます。試用なら `http://localhost:3000` で十分です。実際のインスタンスなら `https://social.example.com` を使います。
- `--data-dir` は `redb` ファイル＋アップロード済みメディアを保持します。バックアップできるディレクトリを指定してください。既定は `./data` です。
- `--listen` はバインドアドレスです。`127.0.0.1:3000` でローカルのみにできます。

起動確認：

```bash
curl http://localhost:3000/health   # -> ok
curl http://localhost:3000/ready    # -> ready
```

`Ctrl-C` で停止します。本番では HTTPS 用にリバースプロキシ（Caddy／nginx）を前段に置いてください — birchtree は素の HTTP を話し、`--trusted-proxy=` アドレスからの `X-Forwarded-For` のみを信頼します。

全フラグのリファレンス（`--max-body-size`、`--max-status-chars`、`--site-title/description/contact`、`--registration-open/closed`、`--streaming-url`、`--federation-*`、`RUST_LOG`）：`--help` と、前回 README 改訂版の表／`src/config.rs` を参照してください。

### 最初のアカウントを作る

登録は既定で**閉鎖**されているため、CLI から最初のアカウントをブートストラップします：

```bash
./target/release/birchtree create-account \
  --data-dir="$HOME/.local/share/birchtree" \
  --public-url="http://localhost:3000" \
  --username=alice \
  --password='a-strong-password' \
  --email='alice@example.com' \
  --display-name='Alice'
```

`created account id=... username=alice` と出力されます。そのアカウントが管理者です。パスワードは `--password=` の代わりに `BIRCHTREE_PASSWORD` から渡すこともできます。

登録を開放するには：

```bash
./target/release/birchtree \
  --data-dir="$HOME/.local/share/birchtree" \
  --listen="0.0.0.0:3000" \
  --public-url="http://localhost:3000" \
  --registration-open
```

### ログインして投稿する

任意の Mastodon クライアントを公開 URL に向けるか、直接 API を使います：

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

インスタンス情報：`GET /api/v1/instance` と `GET /api/v2/instance`。

---

## 日常の使い方

起動後は、どの Mastodon インスタンスとも同様に操作します（フォームエンコード、マルチパート、または JSON ボディ。ID は decimal 文字列。カーソルは `Link` ヘッダー付きの排他的数値 `max_id`／`since_id` です）：

- **投稿：** `status`、`visibility`（`public`／`unlisted`／`private`／`direct`）、`spoiler_text`、`in_reply_to_id`、`media_ids[]`、`poll[options][]`、`scheduled_at`、`quote` フィールド付きで `POST /api/v1/statuses`。編集は `PUT /api/v1/statuses/:id`、削除は `DELETE /api/v1/statuses/:id`。
- **タイムライン：** `GET /api/v1/timelines/home|public`、`/api/v1/timelines/tag/:tag`、`/api/v1/timelines/list/:id`、`/api/v1/accounts/:id/statuses`。
- **操作：** `/api/v1/statuses/:id/{favourite,unfavourite,reblog,unreblog,bookmark,unbookmark,pin,unpin}`、`/api/v1/polls/:id/votes`、`/api/v1/follows`、`/api/v1/blocks|mutes|domain_blocks`。
- **メディア：** `POST /api/v1/media`／`POST /api/v2/media`（マルチパート）。ファイルはスニッフィングしたコンテントタイプでローカル配信されます。
- **通知／マーカー：** `/api/v1/notifications`、`/api/v1/markers`。
- **リスト／フィルター：** `/api/v1/lists`、`/api/v1/filters`（v1＋v2）。
- **検索：** `GET /api/v2/search`（ローカルコーパスのみ）。
- **ストリーミング：** OAuth トークン付きでストリーミングエンドポイント上の SSE または WebSocket。ストリームは `user`、`user:notification`、`public`、`hashtag`、`list`。

完全な対象範囲は `docs/endpoint-manifest.md` にあります。プロファイル外のものは Mastodon の `{ "error": "..." }` 形状で `404` を返します（Misskey のエンベロープではありません）。

---

## 連携相手

birchtree は ActivityPub で連合し、プライベートなラボでのみテストされています — 公開インターネットに対しては一度もテストしていません：

- **Mastodon 4.7.3**（ピン留めコミット `8ff11c2b`）：5 サーバーベンチ（自身、pinetree、Mastodon）でフォロー、投稿、返信、ブースト、いいね、削除、メディア、投票、引用が通過します。
- **pinetree**（Misskey 系）：コアアクティビティ集合の異種間ベンチが通過します。

Note の Create／Update／Delete、Follow／Accept／Reject／Undo、Like／Undo、Announce／Undo、Block／Undo、Flag、Question＋投票、引用 Request／Accept／Reject／Revoke を話し、さらに WebFinger、NodeInfo 2.0、HTTP Signatures を扱います。連合入力は信頼できないものとして扱います（サイズ／タイムアウト／リダイレクト制限、SSRF 防御）。

---

## プロジェクト構成

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

依存方向は `domain` ← `application` ← アダプタです。REST と ActivityPub は同じアプリケーション操作を呼びます — ビジネスロジックの重複はありません。連合自体は `activitypub-federation-rust` に委譲しています。`federation/` はポリシー／配送であり、独自プロトコル実装ではありません。

---

## 自分でテストする

完全な検証ゲートは以下です：

```bash
cargo check --all-targets      # must be warning-free
cargo test                     # full suite (currently 349 passed / 0 failed --release)
./scripts/smoke-all.sh         # check + tests + phase smoke scripts
```

個別に実行することもできます（`cargo test <name>`）。完全なガイドは `docs/testing.md` を参照してください。

実際の Mastodon に対して連合をテストするには `scripts/lab/README.md` を参照してください。Mastodon 4.7.3 のプライベート Docker ブリッジを立ち上げます。ラボ用スイッチは既定で無効であり、本番では決して使ってはいけません。

---

## 貢献と関連ドキュメント

- `birchtree.md` — 製品とアーキテクチャの計画書。この README と食い違う場合はこちらが優先されます。
- `AGENTS.md` — 寄稿者（人間・AI 問わず）のための作業ルール。
- `docs/` — 契約群（`endpoint-manifest.md`、`compatibility-profile.md`、`federation-contract.md`、`streaming-contract.md`、…）で、それぞれに `Status:` 行があります。Frozen ファイルは、記録された上書き決定を経てのみ変更されます。
- `docs/wiki/` — 読みやすい概要（About、現在の状況、ロードマップ）。
- `state/logs/`＋`docs/logs/` — 実際の出力を含む日付付き作業ログと検証証跡。最新エントリを読めば追いつけます。

手伝いたい場合は、以下が最も役立ちます：Mastodon 互換クライアントでの試用、テストスイートと連合ラボの実行、issue の報告。トランスコード（7.1）、検索（7.2）、管理／モデレーション（7.3）が未開拓の領域です。

---

## ライセンス

AGPL-3.0 — `LICENSE` を参照してください。
