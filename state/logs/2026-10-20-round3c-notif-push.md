# 2026-10-20 — round 3 area C: notification requests + encrypted push (D20/D42)

Subagent work; all changes left uncommitted for parent merge.

## Upstream verification (read, not guessed)

- Filter → request-queue → merge pipeline: `NotifyService` (`app/services/notify_service.rb:226-260`:
  `drop?` → `filter?` (`FilterCondition`, `filterable_type?` at `:37`,
  `filterable` flags in `app/models/notification.rb:37-103`), filtered rows
  stored with `filtered=true`, `update_notification_request!` (`:255-260`)
  files the request row for mention/quote only) →
  `AcceptNotificationRequestService` (permission + `UnfilterNotificationsWorker`)
  → `app/workers/unfilter_notifications_worker.rb:42` publishes
  `{event: :notifications_merged, payload: '1'}` on the recipient's
  notifications channel. Routes: `config/routes/api.rb:178-194` (requests
  index/show, member + bulk accept/dismiss, collection `merged?`,
  singleton `notifications/policy`).
- Policy defaults (`app/models/notification_policy.rb` schema comments):
  `for_private_mentions` + `for_limited_accounts` default `filter`, the rest
  `accept`. Thresholds: `NEW_FOLLOWER_THRESHOLD = 3.days`,
  `NEW_ACCOUNT_THRESHOLD = 30.days` (`notify_service.rb:22-23`).
- Push: `app/workers/web/push_notification_worker.rb:64-80`
  (`perform_standard_request`, `Content-Encoding: aes128gcm`); plaintext is
  the `Web::NotificationSerializer` JSON
  (`{access_token, preferred_locale, notification_id, notification_type,
  icon, title, body}`, `app/serializers/web/notification_serializer.rb`);
  titles mirror `notification_mailer.*.subject` (`config/locales/en.yml:2079+`).

## What was built

- **Requests pipeline** (`application/notifications.rs`): `evaluate_policy`
  (filterable types, permission bypass, six `for_*` accept/filter/drop arms
  with upstream defaults/thresholds) hooked into `fan_out_from_payload`
  (filtered → hidden row + request upsert, no event/push; dropped → no trace);
  `accept_request` (permission + unfilter + `StreamEvent::NotificationsMerged`
  only when rows actually merged), `dismiss_request` (deletes filtered rows,
  silent), bulk variants, `merged` (= no pending requests), V1 policy
  get/set. New endpoints in `endpoints/notifications.rs` + routes in
  `router.rs` (show/member/bulk accept+dismiss/policy singleton).
- **Storage** (`domain/notification.rs`: `filtered` flag `#[serde(default)]`,
  `PolicyAction`, `NotificationPolicy`, `NotificationRequest`;
  `ports.rs`: `NewNotification.filtered` + 12 `NotificationStore` methods;
  `redb/notifications.rs`: impl + index/count exclusion of filtered rows;
  `schema.rs`: 4 tables). Read paths tolerate absent tables (fresh/legacy DBs).
- **Event**: `StreamEvent::NotificationsMerged` (`application/streaming.rs`)
  + fanout arm delivering raw `"1"` to `user` (notif scope) +
  `user:notification` (`streaming/fanout.rs`).
- **Encrypted push** (new `application/push_crypto.rs`, `ring`-only, no new
  dep): RFC 8291 §3.4 KDF (`auth_secret` HKDF salt; `WebPush: info` context),
  AES-128-GCM single record (`0x02` delimiter, `rs=4096`); `push.rs` sends it
  for `standard` subs, tickle preserved for legacy. Payload shape =
  upstream serializer with `access_token:""` (unrecoverable server-side) and
  `icon:""` (no avatar model) — documented, never fabricated.

## Verification (executed)

- `cargo test --test round3_notif_push`: **6/6 pass** (request→merge→event on
  both streams; no-request silence; dismiss silence; encrypt round-trip;
  full `handle_push_job` delivery decrypt-check; tickle preserved).
- No regressions: full `cargo test` green except (a)
  `phase7_conversations_push::push_job_end_to_end_through_fake_endpoint` —
  legitimate upstream-faithful interaction (default `private_mentions=filter`
  now filters the test's stranger DM; parent patch snippet in handoff reply),
  and (b) `phase7_1_transcode::decision_table_per_input_class` — subagent B's
  in-flight media work, untouched here. D20 lock test
  `d20_notifications_merged_is_never_sent` still passes (default policy +
  public mention → accept path, no event).
- `cargo check --all-targets`: no warnings from area-C files.

## Open questions for parent

- Merge the D20/D42 register + endpoint-manifest snippets (in handoff reply).
- Replace/extend the D20 lock test (proposal in handoff reply).
- Patch the one push test (snippet in handoff reply).
- Optional: add the 4 tables to the migrations v1 table inventory.
- Lab interop of encrypted payloads against a live push service still pending.
