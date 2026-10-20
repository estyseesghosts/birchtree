# 2026-10-20 — fix-4 P1/P3: instance sub-resources, donation absence, metrics gate, admin identity stores

Subagent work (owns: `instance.rs`, `donation_campaigns.rs`, `admin_infra.rs`,
`application/admin.rs`, `application/admin_infra.rs`, `tests/fix4_instance_admin.rs`).

## Upstream verification (pin `8ff11c2b`, working tree NOT read)

- Instance sub-resources (`config/routes/api.rb:144-157`): `peers`, `rules`,
  `domain_blocks` (index); `terms_of_service` (index+show, `param: :date`);
  `privacy_policy`, `extended_description`, `translation_languages`,
  `languages`, `activity` (show). V2 has show only (`api.rb:351`).
- Peers: `Instance.searchable.pluck(:domain)`, 404 unless
  `peers_api_enabled && !limited_federation_mode` (`peers_controller.rb`);
  defaults true (`config/settings.yml:29`).
- Rules: `Rule.ordered` (`priority, id`) + `REST::RuleSerializer`
  (`id/text/hint/translations`).
- Languages: `SUPPORTED_LOCALES` keys + `LanguagePresenter`
  (`code`/`name`); no locale catalog exists locally → serve distinct status
  languages with `name` = code (registered divergence).
- Activity: 12 trailing windows `{week, statuses, logins, registrations}`
  as strings; 404 unless `activity_api_enabled` (default true,
  `settings.yml:28`).
- Domain blocks: `with_user_facing_limitations` (silence/suspend) ordered
  `by_severity`; `comment` only with rationale setting; upstream default
  `show_domain_blocks: 'disabled'` → 404 (`settings.yml:30`).
- Extended description unset → `{updated_at: null, content: ''}`
  (serializer `&.` + blank text). Translation languages unconfigured → `{}`.
  Privacy policy always present upstream via bundled default template (not
  bundled here → 404). Terms index 404s with no current terms
  (`RecordNotFound`).
- Donation: real outbound fetch of operator URL; 204 when unconfigured or
  fetch yields nothing (`donation_campaigns_controller.rb:13-22`).
- Measures/dimensions/retention: `Admin::PermissionsConcern`
  (moderator-or-admin via `Admin::BasePolicy#access?`: administrator OR any
  moderation/administration/devops flag) + per-action
  `authorize :dashboard, :index?` (`DashboardPolicy#index?` =
  `can?(:view_dashboard)`, held by default moderator via moderation
  category, `user_role.rb:64-77`).
- Admin account serializer: `locale` = `user_locale`,
  `invite_request` = `user.invite_request.text`,
  `invited_by_account_id` = `user.invite.user.account_id`, `ip`/`ips` from
  user sign-in IPs (`account_serializer.rb`); filter `invited_by` via
  `Invite.where(user_id:)` (`account_filter.rb:3-13,106-108`).

## Changed

- `instance.rs`: 9 sub-resource handlers (peers/rules/languages/activity
  served from existing scans; domain_blocks 404 by upstream default with a
  real serving branch behind the flag; extended_description/translation
  exact unset shapes; privacy_policy/terms 404).
- `donation_campaigns.rs`: 204 stub → 404 after the user gate.
- `admin_infra.rs`: new `require_metrics` (admin-or-moderator) for
  measures/dimensions/retention; everything else stays admin-only.
- `application/admin.rs`: `parse_invited_by` + `inviter_account_id`
  (InviteStore-backed creator resolution for the serializer field/filter).
- `tests/fix4_instance_admin.rs`: 9 tests, all passing
  (`cargo test --test fix4_instance_admin`: 9 passed).

## Open (parent-owned files; proposals returned, not applied)

- Router mount snippet, `Config` flag additions, manifest rows, `LocalUser`
  locale field, `INVITE_USES`/`SIGN_IN_IPS` store + `Store`-trait snippets,
  `admin_accounts.rs` serializer/filter wiring, `dto.rs` v2
  `rules`/`languages` un-hardcoding, `auth.rs` doc touch-up.
