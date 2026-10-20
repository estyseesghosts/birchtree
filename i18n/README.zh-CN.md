# birchtree

> 本项目迭代迅速，本文档可能很快就会过时。项目根目录下的英文 README 通常更新更及时。

birchtree 是一个单进程、**兼容 Mastodon API 的 fediverse 服务器**，
用 Rust 编写。它重新实现了 [Mastodon **4.7.3**](https://github.com/mastodon/mastodon/blob/v4.7.3/CHANGELOG.md)
（API v7），后端为内嵌 `redb` 与本地磁盘媒体。联邦部分通过
[`activitypub-federation-rust`](https://github.com/LemmyNet/activitypub-federation-rust)
讲 ActivityPub，再加上 birchtree 自身的策略与投递规则。

它的设计目标就是**运行起来很小**：不需要 Postgres、Redis 或消息中间件。
一个二进制文件、一个数据目录（`redb` 文件 + 媒体），仅此而已。

birchtree 不自带前端。用任意兼容 Mastodon 的客户端指向它即可——参见
[keystone](https://github.com/estyseesghosts/keystone)（开发中）。官方
Mastodon 前端尚未在它上面测试过，结果如何请自行验证。

> **状态（2026-10-20）：**本地可用，已在实验环境中实现联邦
> （`cargo test --release`：349 通过 / 0 失败）。账号、状态、
> 关系、媒体、时间线、通知/标记/列表/过滤器、
> 流式推送（SSE + WebSocket）以及核心联邦功能均已实现，
> 并已针对真实 Mastodon 4.7.3 在实验环境中验证。会话、私信推送、管理
> API、`/api/web/*`、转码、全文搜索和多节点模式
> 尚在规划中，未实现。仅在模拟环境和私有实验环境中测试过——
> 从未对接过线上 fediverse。

---

## 目录

- [目前已实现的功能](#what-it-does-today)
- [暂未实现的功能](#what-it-does-not-do)
- [已知差距与粗糙之处](#known-gaps-and-rough-edges)
- [项目进展](#where-things-stand)
- [运行你自己的服务器](#run-your-own-server)
  - [前置条件](#prerequisites)
  - [构建](#build-it)
  - [启动](#start-it)
  - [创建第一个账号](#create-your-first-account)
  - [登录并发帖](#log-in-and-post)
- [日常使用](#use-it-day-to-day)
- [可互联的对象](#what-it-talks-to)
- [项目结构](#project-layout)
- [自行测试](#testing-it-yourself)
- [贡献与延伸阅读](#contributing--reading-more)
- [许可证](#license)

---

## 目前已实现的功能

本节中的所有内容都运行在单节点服务器上，并有已执行的测试覆盖。
未实现的路由返回干净的 `404`——绝不伪造数据——
下文注明的情况除外（因数据源尚不存在而正确返回空的端点）。
确切的契约见 `docs/endpoint-manifest.md` 与 `docs/compatibility-profile.md`
（差异登记 D1–D18）。

**账号、认证与注册：**

- OAuth2 应用注册（`POST /api/v1/apps`）、带 PKCE S256 的授权码流程、
  client-credentials 应用、令牌撤销、按路由的 scope 强制校验、
  服务器端渲染的登录/授权页面。
- 注册策略（开放/关闭）、CLI 初始化账号。邮件发送仅为打日志的桩——
  不做强制校验。
- 兼容 Mastodon 的十进制 ID：64 位面向时间戳的 ID，序列化为
  十进制字符串；内部按数值比较，绝不按字典序比较。

**状态与互动：**

- 发布、查看、编辑（含历史记录）、删除、回复 + 上下文、转帖、
  喜欢、书签、置顶（上限 5）、`favourited_by`/`reblogged_by`、
  源文本查看、多条拉取、`Idempotency-Key` 重放（保留 1 小时）。
- 引用作为本地状态机：策略（`public`/`followers`/`nobody`）、
  接受/撤销、互动策略端点、`quotes_count`。
- 本地投票（创建/投票/过期、总数隐藏）与定时状态
  （含 `<5 min` 422 对齐行为）。
- 状态正文长度默认上限 500 字符（与 Mastodon 对齐），可通过
  `--max-status-chars=` 配置到 3000。

**关系：**

- 关注（锁定账号 → 待处理的请求，可批准/拒绝）、
  取关、从粉丝中移除、拉黑、免打扰（含过期时间）、域名屏蔽
  （上游 v4.7.3 结构）、背书、账号备注、粉丝/关注列表、
  按查看者聚合的关系数据。

**媒体：**

- v1/v2 multipart 上传（含 MIME 嗅探）、处理生命周期
  （`pending → ready`、`202`/`206` 语义）、描述/焦点更新、
  本地文件服务（使用嗅探到的内容类型）。

**时间线与搜索：**

- 主页、公共（`local`/`remote`/`only_media`）、话题、列表与账号
  时间线，使用排他性数值游标与 `Link` 头。
- 搜索为仅针对本地语料的 `GET /api/v2/search`
  （账号/状态/话题）——没有全文检索引擎。

**通知、标记、列表、过滤器：**

- 通知索引/查看/清空/关闭/`unread_count`、
  `GET`+`POST /api/v1/markers`；完整的列表 CRUD + 成员管理；v1+v2
  过滤器（含 `filtered` 标注）。
- Phase 7 已带入：账号多条拉取（`?id[]=`，上限 40）、查找
  与搜索；`POST /api/v1/reports`（按要求存储 + 转发 `Flag`）；
  推荐与公告返回 `[]`；捐赠活动返回 `204`。

**流式推送：**

- 进程内 SSE 与 WebSocket，覆盖 `user`、`user:notification`、`public`
  （+`local`/`remote`/仅媒体变体）、`hashtag`（+`local`）与 `list`，
  带认证/scope、心跳与显式上限（共 512 条、每 token 16 条、
  溢出时 fail-closed）。
- 事件：`update`、`delete`、`status.update`、`notification`。

**联邦（ActivityPub）：**

- WebFinger（`acct:` 解析）、NodeInfo 2.0、actor/密钥文档、共享 +
  个人收件箱、发件箱/粉丝/关注集合、带签名投递
  （重试/退避/隔离）、SSRF 安全的拉取（大小/超时/重定向
  限制、DNS/IP 防护）。
- 双向活动：Follow/Accept/Reject/Undo、Note 的 Create/Update/Delete、
  Like/Undo、Announce/Undo（含删转帖时的 Undo）、
  Block/Undo、投票 Question + 投票、引用的 Request/Accept/Reject/Revoke、
  Flag 发送/接收、远端媒体拉取（原始字节按原样缓存）。
- 话题补全：入库时解析父状态（每任务一跳、跳数上限
  + 拉取预算，绝不提升为根）加带认证的上下文抓取
  （`Mastodon-Async-Refresh`）。

**持久性：**

- 一个 `redb` 事务原子提交状态变更 + 索引/计数器 +
  幂等/去重记录 + 有界持久化任务。网络 I/O、
  媒体处理、通知扇出与投递都在提交后进行。
  Worker 幂等；崩溃恢复测试证明已提交的变更
  不会丢失其任务。

---

## 暂未实现的功能

刻意的非目标与延期工作——请求这些将返回 `404`
（或下文注明的空/204），而不是残缺的半成品：

- **无会话/私信 UI 界面。**无 `GET /api/v1/conversations`
  系列、无会话免打扰。私信作为状态联邦，但
  没有话题串联，也没有 `direct` 流事件。
- **无 Web 推送。**无推送订阅与投递（规划中的 Phase 7）。
- **无管理与审核 API。**无 `/api/v1/admin/*` 或 `/api/v2/admin/*`、
  无角色（CLI 账号无角色）、无工单队列 UI、无趋势、无
  年度报告（规划中的 Phase 7.3）。
- **无第一方 Web 客户端路由。**无 `/api/web/*`、不托管 keystodon 包
  （规划中的 Phase 7）。
- **无媒体转码。**无 GIF→视频、无 VP9/WebM、无 WebP、无 `gifv`
  （规划中的 Phase 7.1）。
- **无真正的搜索。**仅本地语料扫描；无内嵌全文检索引擎、
  无 Elasticsearch（规划中的 Phase 7.2）。
- **无多节点模式。**仅单节点；无 Postgres/Redis/FoundationDB
  后端，无共享存储媒体方案。
- **尚未实现：**邮件确认强制、邀请/审批
  注册模式、刷新令牌（`unsupported_grant_type`）、`GET
  blocks/mutes/favourites/bookmarks` 集合读取、`notifications_merged`
  事件、通知 `requests` 管线（恒为 `[]`）、已选语言
  过滤。

---

## 已知差距与粗糙之处

这些功能可用，但有文档化的限制，你可能会踩到：

- **仅在实验环境中测试过。**互操作性只在私有 Docker 桥接
  实验环境中针对真实 Mastodon 4.7.3 验证过，从未对接过
  线上公网。实验开关（`--federation-allow-http`、`--federation-allow-private`、
  `--federation-extra-ca`）默认关闭，绝不能在
  生产环境中使用。
- **与 Mastodon 4.7.x 的媒体交换在实验环境中不稳定**
  （Mastodon 侧的拉取/重编码行为；见
  `docs/federation-contract.md`）。尚无按目标的投递冷却
  （退避按任务计算）。Delete 与 Create 竞态时可能留下残留
  （需要 tombstone——唯一已知的 birchtree 自身问题）。
- **媒体差异（已接受）：**动态 GIF 保持 `image`，永不转为 `gifv`
  （D1）；视频/音频原样透传（D2–D3）；静态图不转为
  WebP（D4）。Phase 7.1 会改打包器（`type`、`url`、`preview_url`、
  `meta`、待处理时为 null）。
- **时间线差异（已接受）：**主页只读最近 1000 个
  关注（D10a）；排他性列表成员在主页中隐藏（D10b）；主页无 `206`
  异步刷新（D9）。
- **话题串差异（D5）：**读取上下文时仅登录用户可拉取祖先——
  Mastodon 没有这个限制。
- **`direct` 流已接受但保持静默**，直到会话功能存在（D18）。
- **单节点存储。**单个写者（`redb`），无只读副本，无
  到多节点的在线迁移路径。
- **可观测性最简**（结构化日志、`RUST_LOG`、健康/就绪检查
  而已）。无按目标的投递调优 UI。

---

## 项目进展

| 阶段 | 内容 | 状态 |
|---|---|---|
| 0 | 冻结源契约（相对 v4.7.3 的路由清单、DTO、可见性、端口） | 评审中——路由清单评审未关闭 |
| 1 | 运行时、`redb`、ID、配置、初始化 | 完成 |
| 2 | OAuth、注册、精确读 API | 完成 |
| 3 | 本地状态、关系、引用、幂等 | 完成 |
| 4 | 媒体、时间线、通知、列表/过滤器/搜索 | 完成 |
| 4.1 | 差异收口（JSON 请求体 D11、排他性列表 D10b、主页上限 D10a） | 完成 |
| 5 | 流式推送（SSE + WebSocket、信封、边界） | 当前——基本建成，退出评审未关闭 |
| 6 | ActivityPub 联邦（收件箱、投递、话题补全） | 核心已落地 + 实验环境验证；加固未关闭 |
| 7 | 转码（7.1）、搜索（7.2）、管理/审核（7.3）、推送、`/api/web/*` | 规划中——仅多条拉取/举报/桩已落地 |
| 8 | 声明的 API 兼容性（98% 对齐套件） | 规划中 |

单节点服务器**今天本地可用**。在 Phase 8 对齐门禁通过之前，
不声明兼容 v4.7.3。

---

## 运行你自己的服务器

### 前置条件

1. **Rust 工具链**（stable，edition 2024），经 [rustup](https://rustup.rs/) 安装。
2. **C 工具链**（gcc/clang），用于加密依赖。Fedora：
   `sudo dnf install gcc`。Debian/Ubuntu：`sudo apt install build-essential`。
3. `curl`，用于调试 API。

```bash
rustc --version   # 1.98+ recommended
cargo --version
```

### 构建

```bash
git clone https://github.com/<you>/birchtree.git
cd birchtree
cargo build --release
```

这会在 `target/release/birchtree` 产出单个二进制文件
（`cargo build` 对应 `target/debug/birchtree`）。

### 启动

```bash
mkdir -p ~/.local/share/birchtree
./target/release/birchtree \
  --data-dir="$HOME/.local/share/birchtree" \
  --listen="0.0.0.0:3000" \
  --public-url="http://localhost:3000"
```

- `--public-url` 是外界（和客户端）看到的 URL。`http://` 或
  `https://` 加主机名。Actor/对象 URI、WebFinger 与 NodeInfo 都
  由它构建。`http://localhost:3000` 适合折腾；
  真实站点用 `https://social.example.com`。
- `--data-dir` 存放 `redb` 文件 + 上传的媒体。指向一个
  你能备份的目录。默认 `./data`。
- `--listen` 是绑定地址。`127.0.0.1:3000` 表示仅本机。

检查是否存活：

```bash
curl http://localhost:3000/health   # -> ok
curl http://localhost:3000/ready    # -> ready
```

用 `Ctrl-C` 停止。生产环境请在前面放反向代理（Caddy/nginx）
做 HTTPS——birchtree 只讲明文 HTTP，且仅信任来自
`--trusted-proxy=` 地址的 `X-Forwarded-For`。

完整 flag 参考（`--max-body-size`、`--max-status-chars`、
`--site-title/description/contact`、`--registration-open/closed`、
`--streaming-url`、`--federation-*`、`RUST_LOG`）：见 `--help` 及
上一版 README 中的表格 / `src/config.rs`。

### 创建第一个账号

注册默认**关闭**，所以先从 CLI 初始化第一个账号：

```bash
./target/release/birchtree create-account \
  --data-dir="$HOME/.local/share/birchtree" \
  --public-url="http://localhost:3000" \
  --username=alice \
  --password='a-strong-password' \
  --email='alice@example.com' \
  --display-name='Alice'
```

它会打印 `created account id=... username=alice`。该账号即你的
管理员账号。密码也可用 `BIRCHTREE_PASSWORD` 传入，代替 `--password=`。

开放注册：

```bash
./target/release/birchtree \
  --data-dir="$HOME/.local/share/birchtree" \
  --listen="0.0.0.0:3000" \
  --public-url="http://localhost:3000" \
  --registration-open
```

### 登录并发帖

将任意 Mastodon 客户端指向你的公开 URL，或直接调用 API：

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

实例信息：`GET /api/v1/instance` 与 `GET /api/v2/instance`。

---

## 日常使用

跑起来之后，像用任何 Mastodon 实例一样交互（form 编码、multipart
或 JSON 请求体；ID 为十进制字符串；游标为排他性数值
`max_id`/`since_id`，配 `Link` 头）：

- **发帖：**`POST /api/v1/statuses`，带 `status`、`visibility`
  （`public`/`unlisted`/`private`/`direct`）、`spoiler_text`、`in_reply_to_id`、
  `media_ids[]`、`poll[options][]`、`scheduled_at`、`quote` 字段。用
  `PUT /api/v1/statuses/:id` 编辑，用 `DELETE /api/v1/statuses/:id` 删除。
- **时间线：**`GET /api/v1/timelines/home|public`、`/api/v1/timelines/tag/:tag`、
  `/api/v1/timelines/list/:id`、`/api/v1/accounts/:id/statuses`。
- **互动：**`/api/v1/statuses/:id/{favourite,unfavourite,reblog,unreblog,bookmark,unbookmark,pin,unpin}`、
  `/api/v1/polls/:id/votes`、`/api/v1/follows`、`/api/v1/blocks|mutes|domain_blocks`。
- **媒体：**`POST /api/v1/media` / `POST /api/v2/media`（multipart）；文件
  在本地托管，使用嗅探到的内容类型。
- **通知/标记：**`/api/v1/notifications`、`/api/v1/markers`。
- **列表/过滤器：**`/api/v1/lists`、`/api/v1/filters`（v1+v2）。
- **搜索：**`GET /api/v2/search`（仅本地语料）。
- **流式推送：**在流式端点上用 SSE 或 WebSocket，带 OAuth 令牌；
  流为 `user`、`user:notification`、`public`、`hashtag`、`list`。

完整接口见 `docs/endpoint-manifest.md`。不在画像内的任何内容
返回 `404`，使用 Mastodon 的 `{ "error": "..." }` 结构（不是 Misskey
信封）。

---

## 可互联的对象

birchtree 经 ActivityPub 联邦，已在私有实验环境中测试过——
从未对接过公网：

- **Mastodon 4.7.3**（pinned commit `8ff11c2b`）：在五服务器基准
  （自身、pinetree、Mastodon）中，关注、发帖、回复、
  转帖、喜欢、删除、媒体、投票、引用均通过。
- **pinetree**（Misskey 系）：核心活动集的跨系基准通过。

它讲 Create/Update/Delete Note、Follow/Accept/Reject/Undo、Like/Undo、
Announce/Undo、Block/Undo、Flag、Question + 投票，以及引用的
Request/Accept/Reject/Revoke，外加 WebFinger、NodeInfo 2.0 与 HTTP
签名。联邦输入一律视为不可信（大小/超时/重定向
限制、SSRF 防护）。

---

## 项目结构

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

依赖方向为 `domain` ← `application` ← 适配器。REST 与
ActivityPub 调用同一套应用层操作——业务逻辑不重复。
联邦本身委托给 `activitypub-federation-rust`；
`federation/` 只做策略/投递，不是自研协议实现。

---

## 自行测试

完整验证门禁为：

```bash
cargo check --all-targets      # must be warning-free
cargo test                     # full suite (currently 349 passed / 0 failed --release)
./scripts/smoke-all.sh         # check + tests + phase smoke scripts
```

也可以逐块运行（`cargo test <name>`）。完整指南见
`docs/testing.md`。

要用真实 Mastodon 测试联邦，见 `scripts/lab/README.md`。它会
拉起带 Mastodon 4.7.3 的私有 Docker 桥接网络。实验开关默认
关闭，绝不能在生产环境中使用。

---

## 贡献与延伸阅读

- `birchtree.md`——产品与架构规划。与本 README 有任何
  不一致时以它为准。
- `AGENTS.md`——贡献者（人类或 AI）的行事规则。
- `docs/`——契约（`endpoint-manifest.md`、`compatibility-profile.md`、
  `federation-contract.md`、`streaming-contract.md`、……），每个文件带
  `Status:` 行。Frozen 文件仅经记入日志的替代决策变更。
- `docs/wiki/`——可读的概览（About、当前状态、路线图）。
- `state/logs/` + `docs/logs/`——带真实输出的日期化工作日志与验证证据。
  先读最新一篇即可快速跟上。

如果想帮忙，最有用的事是：用兼容 Mastodon 的客户端实际试用、
跑测试套件与联邦实验环境、
提 issue。转码（7.1）、搜索（7.2）与管理/审核
（7.3）是开放的前沿方向。

---

## 许可证

AGPL-3.0——见 `LICENSE`。
