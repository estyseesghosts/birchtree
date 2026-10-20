//! Web Push subscriptions and delivery.
//!
//! Upstream reference: Mastodon v4.7.3 (`8ff11c2b`)
//! `app/controllers/api/v1/push/subscriptions_controller.rb`,
//! `app/models/web/push_subscription.rb`,
//! `app/serializers/rest/web_push_subscription_serializer.rb`,
//! `app/workers/web/push_notification_worker.rb`.
//!
//! A subscription is bound to the access token that created it (one per
//! token; creating replaces). Delivery is a durable post-commit job
//! ([`crate::domain::JobKind::WebPushDelivery`], parent patch): a
//! VAPID-authenticated RFC 8030 POST with an empty body (a "tickle" — the
//! client wakes and fetches; see the delivery deviation below), bounded
//! retries, then quarantine. A `404`/`410` answer deletes the subscription
//! (upstream removes gone endpoints the same way). Network I/O never runs
//! inside a request transaction.
//!
//! Delivery (round 3, area C closes the encryption half): standard
//! subscriptions get an RFC 8291 `aes128gcm`-encrypted JSON payload (the
//! `Web::NotificationSerializer` shape — see `push_notification_payload` for
//! the two documented field deviations) under the same VAPID identity and
//! the same TTL/urgency, with the same policy/alert gates and the same
//! gone-endpoint cleanup. Legacy (`!standard`) subscriptions keep the
//! VAPID tickle: the wire stays RFC 8030-compliant (a push message MAY have
//! no body), at the cost that those clients cannot render the notification
//! without fetching. Upstream's legacy `aesgcm` encoding is not implemented
//! (recorded in the D42 register note).

use std::collections::HashMap;

use crate::domain::{Error, Id, Notification, Result, WebPushSubscription};

/// Store surface for push subscriptions bound to access tokens.
///
/// Implemented once by the `redb` adapter (parent patch). The parent also
/// registers this trait as a supertrait of [`crate::application::ports::Store`].
pub trait PushSubscriptionStore: Send + Sync {
    /// The subscription bound to `token_id`, if any.
    fn push_subscription_by_token(&self, token_id: Id) -> Result<Option<WebPushSubscription>>;

    /// The subscription with numeric `id`, if any (delivery job input).
    fn push_subscription_by_id(&self, id: Id) -> Result<Option<WebPushSubscription>>;

    /// Every subscription of `user_id` (fan-out input; upstream scopes
    /// subscriptions to the token, and tokens to the user).
    fn push_subscriptions_for_user(&self, user_id: Id) -> Result<Vec<WebPushSubscription>>;

    /// Create or replace the subscription for `(token_id, user_id)` in ONE
    /// transaction (upstream destroys the token's subscriptions on create, so
    /// exactly one row exists per token).
    fn upsert_push_subscription(
        &self,
        token_id: Id,
        user_id: Id,
        endpoint: String,
        key_p256dh: String,
        key_auth: String,
        standard: bool,
        policy: String,
        alerts: HashMap<String, bool>,
        now_ms: u64,
    ) -> Result<WebPushSubscription>;

    /// Replace only the `data` (policy/alerts) of the token's subscription.
    /// `None` when the token has none.
    fn update_push_subscription_data(
        &self,
        token_id: Id,
        policy: String,
        alerts: HashMap<String, bool>,
        now_ms: u64,
    ) -> Result<Option<WebPushSubscription>>;

    /// Delete the token's subscription. Returns `true` when one existed.
    fn delete_push_subscription(&self, token_id: Id) -> Result<bool>;

    /// Delete the subscription with numeric `id` (gone-endpoint cleanup).
    /// Returns `true` when one existed.
    fn delete_push_subscription_by_id(&self, id: Id) -> Result<bool>;
}

/// VAPID identity, from configuration (parent patch adds
/// `vapid_public_key` / `vapid_private_key` to `Config`, mirroring upstream's
/// `VAPID_PUBLIC_KEY` / `VAPID_PRIVATE_KEY`).
///
/// Both keys are base64url (no padding): the 65-byte uncompressed P-256
/// point and the 32-byte private scalar. Operators generate a pair with
/// `openssl ecparam -genkey -name prime256v1 -noout` (DER) and convert, or
/// reuse the pair from a previous Mastodon install; either form is accepted
/// as long as it decodes to the right lengths.
#[derive(Debug, Clone)]
pub struct VapidKeys {
    pub public_b64: String,
    pub private_b64: String,
}

impl VapidKeys {
    /// Whether delivery is configured at all (both halves present).
    pub fn is_configured(&self) -> bool {
        !(self.public_b64.trim().is_empty() || self.private_b64.trim().is_empty())
    }
}

/// VAPID key rotation story (operator decision, recorded here):
///
/// The public key is advertised to clients (`server_key` on the subscription
/// serializer and `vapid.public_key` on `/api/v2/instance`) and baked into
/// `Authorization: vapid k=...` on every delivery. Rotating the pair
/// therefore invalidates outstanding subscriptions the same way it does
/// upstream: clients detect the mismatch (a `410`/`404` on delivery deletes
/// the row here) and re-subscribe with keys derived from the new `server_key`.
/// Rotation is a restart with the new pair; no dual-key overlap is kept.
/// An unconfigured (empty) pair does not fail startup: subscription endpoints
/// keep working, and delivery jobs quarantine with a configuration error
/// until keys are set.
pub const VAPID_JWT_LIFETIME_SECS: u64 = 12 * 3600;

/// Push TTL in seconds (upstream `Web::PushNotificationWorker::TTL`, 48 h).
pub const PUSH_TTL_SECS: u64 = 48 * 3600;

/// Build the VAPID JWT for `audience_origin` (`scheme://host[:port]` of the
/// push endpoint), signed ES256 with the configured private key.
pub fn vapid_jwt(
    keys: &VapidKeys,
    audience_origin: &str,
    subject: &str,
    now_ms: u64,
) -> std::result::Result<String, String> {
    use base64::Engine as _;
    let enc = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let private_raw = enc
        .decode(keys.private_b64.trim())
        .map_err(|err| format!("vapid private key: {err}"))?;
    let public_raw = enc
        .decode(keys.public_b64.trim())
        .map_err(|err| format!("vapid public key: {err}"))?;
    if private_raw.len() != 32 {
        return Err("vapid private key must decode to 32 bytes".to_string());
    }
    if public_raw.len() != 65 || public_raw[0] != 0x04 {
        return Err("vapid public key must decode to a 65-byte uncompressed point".to_string());
    }
    let rng = ring::rand::SystemRandom::new();
    let pair = ring::signature::EcdsaKeyPair::from_private_key_and_public_key(
        &ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING,
        &private_raw,
        &public_raw,
        &rng,
    )
    .map_err(|err| format!("vapid key pair: {err:?}"))?;
    let header = enc.encode(r#"{"typ":"JWT","alg":"ES256"}"#);
    let claims = serde_json::json!({
        "aud": audience_origin,
        "exp": now_ms / 1000 + VAPID_JWT_LIFETIME_SECS,
        "sub": subject,
    });
    let payload = enc.encode(
        serde_json::to_vec(&claims).map_err(|err| format!("vapid claims: {err}"))?,
    );
    let message = format!("{header}.{payload}");
    let signature = pair
        .sign(&rng, message.as_bytes())
        .map_err(|err| format!("vapid sign: {err:?}"))?;
    Ok(format!("{message}.{}", enc.encode(signature.as_ref())))
}

/// The `aud` claim and `Crypto-Key`/`Authorization` audience: the endpoint's
/// origin (`scheme://host[:port]`).
pub fn push_audience_origin(endpoint: &str) -> std::result::Result<String, String> {
    let url = url::Url::parse(endpoint).map_err(|err| format!("push endpoint: {err}"))?;
    let host = url.host_str().ok_or_else(|| "push endpoint has no host".to_string())?;
    match url.port() {
        Some(port) => Ok(format!("{}://{host}:{port}", url.scheme())),
        None => Ok(format!("{}://{host}", url.scheme())),
    }
}

/// An RFC 8030 delivery request: VAPID headers, TTL and urgency matching
/// upstream's worker. `body` is empty (a "tickle" — the client wakes and
/// fetches) for legacy (`!standard`) subscriptions; for standard
/// subscriptions it carries the RFC 8291 `aes128gcm` ciphertext with
/// `content_encoding` set (round 3, area C; closes the D42 encryption gap).
#[derive(Debug, Clone)]
pub struct PushRequest {
    pub url: String,
    pub authorization: String,
    pub crypto_key: String,
    pub ttl_secs: u64,
    pub body: Vec<u8>,
    pub content_encoding: Option<String>,
}

pub fn build_push_request(
    subscription: &WebPushSubscription,
    keys: &VapidKeys,
    subject: &str,
    now_ms: u64,
) -> std::result::Result<PushRequest, String> {
    let audience = push_audience_origin(&subscription.endpoint)?;
    let jwt = vapid_jwt(keys, &audience, subject, now_ms)?;
    let public_canonical = {
        use base64::Engine as _;
        let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(subscription_key_trimmed(&keys.public_b64))
            .map_err(|err| format!("vapid public key: {err}"))?;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)
    };
    Ok(PushRequest {
        url: subscription.endpoint.clone(),
        authorization: format!("vapid t={jwt}, k={public_canonical}"),
        crypto_key: format!("p256ecdsa={public_canonical}"),
        ttl_secs: PUSH_TTL_SECS,
        body: Vec::new(),
        content_encoding: None,
    })
}

/// Build the encrypted delivery for a standard subscription (upstream
/// `perform_standard_request`): the same VAPID identity/TTL/urgency as the
/// tickle, but the body is the RFC 8291 `aes128gcm` encryption of
/// `payload_json` under the subscription keys.
///
/// Key errors name only the role and length (never key bytes) and are
/// returned for the caller to map onto subscription cleanup, like
/// upstream's invalid-row destroy.
pub fn build_push_request_encrypted(
    subscription: &WebPushSubscription,
    keys: &VapidKeys,
    subject: &str,
    now_ms: u64,
    payload_json: &[u8],
) -> std::result::Result<PushRequest, String> {
    let mut tickle = build_push_request(subscription, keys, subject, now_ms)?;
    let encrypted = crate::application::push_crypto::encrypt_aes128gcm(
        &subscription.key_p256dh,
        &subscription.key_auth,
        payload_json,
    )?;
    tickle.body = encrypted.body;
    tickle.content_encoding = Some(encrypted.content_encoding.to_string());
    Ok(tickle)
}

fn subscription_key_trimmed(raw: &str) -> &str {
    raw.trim()
}

/// Outcome of one delivery attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushOutcome {
    /// The endpoint accepted (2xx).
    Delivered,
    /// The endpoint is gone (`404`/`410`): delete the subscription.
    Gone,
    /// Retry with backoff (bounded by the job's retry budget, then quarantine).
    Retry(String),
}

/// Classify an HTTP status into a [`PushOutcome`].
pub fn classify_push_response(status: u16) -> PushOutcome {
    if (200..300).contains(&status) {
        PushOutcome::Delivered
    } else if status == 404 || status == 410 {
        PushOutcome::Gone
    } else {
        PushOutcome::Retry(format!("push endpoint answered {status}"))
    }
}

/// Send one delivery through `client` (the safe outbound client in
/// production; any `reqwest::Client` in tests). Tickles go out bodiless;
/// encrypted requests carry `Content-Type: application/octet-stream` and
/// their `Content-Encoding` (upstream's standard request adds the same
/// pair, plus `Authorization`, `Ttl`/`TTL`, `Urgency` and `Unsubscribe-URL`;
/// the unsubscribe URL is a web-route concern and stays out of this
/// transport type).
pub async fn deliver_push(
    client: &reqwest::Client,
    request: &PushRequest,
) -> PushOutcome {
    let mut outgoing = client
        .post(&request.url)
        .header("Authorization", &request.authorization)
        .header("Crypto-Key", &request.crypto_key)
        .header("TTL", request.ttl_secs.to_string())
        .header("Urgency", "normal");
    if let Some(encoding) = &request.content_encoding {
        outgoing = outgoing
            .header("Content-Type", "application/octet-stream")
            .header("Content-Encoding", encoding.clone());
    }
    let response = outgoing.body(request.body.clone()).send().await;
    match response {
        Ok(response) => classify_push_response(response.status().as_u16()),
        Err(err) => PushOutcome::Retry(format!("push send failed: {err}")),
    }
}

/// Build the encrypted-payload plaintext for one notification: the JSON of
/// upstream's `Web::NotificationSerializer`
/// (`app/serializers/web/notification_serializer.rb`):
/// `{access_token, preferred_locale, notification_id, notification_type,
/// icon, title, body}`.
///
/// Two documented deviations (never fabricated):
/// - `access_token` is `""`: the token plaintext is not recoverable
///   server-side (only its hash is stored), so there is nothing honest to
///   embed. Upstream embeds the subscription's token.
/// - `icon` is `""`: this server stores no per-account avatars yet (the REST
///   account DTO always serves the missing-avatar asset), so there is no
///   actor avatar URL to embed. Upstream serves the actor's avatar URL.
/// - `preferred_locale` is `"en"`: no locale is stored on the subscription
///   or user. `title` mirrors `notification_mailer.*.subject` (`en.yml`)
///   with the actor's display name (or username); `body` is the status
///   spoiler/source text truncated to 140 characters (upstream strips the
///   rendered HTML and truncates the same way), falling back to the actor's
///   bio text when there is no status.
pub fn push_notification_payload(
    store: &dyn crate::application::ports::Store,
    notification: &Notification,
) -> std::result::Result<Vec<u8>, Error> {
    let actor = match notification.actor_id {
        Some(actor_id) => store.account_by_id(actor_id)?,
        None => None,
    };
    let name = actor
        .as_ref()
        .map(|account| {
            if account.display_name.trim().is_empty() {
                account.username.clone()
            } else {
                account.display_name.clone()
            }
        })
        .unwrap_or_default();
    let title = match notification.r#type.as_str() {
        "favourite" => format!("{name} favorited your post"),
        "follow" => format!("{name} is now following you"),
        "follow_request" => format!("Pending follower: {name}"),
        "mention" => format!("You were mentioned by {name}"),
        "quote" => format!("{name} quoted your post"),
        "reblog" => format!("{name} boosted your post"),
        "poll" => format!("A poll by {name} has ended"),
        _ => format!("New notification from {name}"),
    };
    let mut body_text = match notification.status_id {
        Some(status_id) => match store.status_by_id(status_id)? {
            Some(status) => status
                .spoiler_text
                .clone()
                .filter(|spoiler| !spoiler.trim().is_empty())
                .unwrap_or(status.source_text.clone()),
            None => String::new(),
        },
        None => String::new(),
    };
    if body_text.trim().is_empty() {
        body_text = actor.as_ref().map(|account| account.note.clone()).unwrap_or_default();
    }
    let body_text: String = body_text.chars().take(140).collect();
    let payload = serde_json::json!({
        "access_token": "",
        "preferred_locale": "en",
        "notification_id": notification.id.to_string(),
        "notification_type": notification.r#type.as_str(),
        "icon": "",
        "title": title,
        "body": body_text,
    });
    serde_json::to_vec(&payload).map_err(|err| Error::Internal(format!("push payload: {err}")))
}

/// What the durable job does after one attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushJobOutcome {
    /// Done (delivered, skipped, or subscription removed).
    Complete,
    /// Delete the named subscription, then done.
    DeleteSubscriptionAndComplete(Id),
    /// Retry with backoff; quarantined after the job's retry budget.
    Retry(String),
}

/// Run one `WebPushDelivery` job: `{subscription_id, notification_id}`.
///
/// Loads both rows, skips stale notifications (older than the 48 h TTL),
/// checks the subscription's policy/alert gates against the notification,
/// then sends: an RFC 8291 `aes128gcm`-encrypted JSON payload for standard
/// subscriptions (round 3, area C), the VAPID tickle for legacy
/// (`!standard`) ones. Missing rows complete silently (upstream rescues
/// `RecordNotFound` the same way).
#[allow(clippy::too_many_arguments)]
pub async fn handle_push_job(
    store: &dyn crate::application::ports::Store,
    client: &reqwest::Client,
    keys: &VapidKeys,
    subject: &str,
    subscription_id: Id,
    notification_id: Id,
    now_ms: u64,
) -> std::result::Result<PushJobOutcome, Error> {
    let subscription = store.push_subscription_by_id(subscription_id)?;
    let Some(subscription) = subscription else {
        return Ok(PushJobOutcome::Complete);
    };
    let notification = store.notification_by_id(notification_id)?;
    let Some(notification) = notification else {
        return Ok(PushJobOutcome::Complete);
    };
    if notification.recipient_id != subscription.user_id {
        return Ok(PushJobOutcome::Complete);
    }
    if notification.created_at_ms.saturating_add(PUSH_TTL_SECS * 1000) < now_ms {
        return Ok(PushJobOutcome::Complete);
    }
    if !keys.is_configured() {
        return Ok(PushJobOutcome::Retry("vapid keys are not configured".into()));
    }
    // Follow-policy gates (upstream `policy_allows_notification?`): `followed`
    // needs the recipient following the actor; `follower` the reverse.
    let (follower, followed_by) = match notification.actor_id {
        Some(actor) => {
            let outgoing = store.pair_state(subscription.user_id, actor, now_ms)?;
            let incoming = store.pair_state(actor, subscription.user_id, now_ms)?;
            (outgoing.following, incoming.following)
        }
        None => (false, false),
    };
    if !crate::application::conversations::subscription_pushable(
        &subscription,
        notification.r#type.as_str(),
        follower,
        followed_by,
    ) {
        return Ok(PushJobOutcome::Complete);
    }
    // A notification whose activity is gone is skipped (upstream checks
    // `activity.present?` before sending).
    if !notification_activity_present(store, &notification)? {
        return Ok(PushJobOutcome::Complete);
    }
    // Standard subscriptions get the encrypted payload (upstream
    // `perform_standard_request`); legacy ones keep the tickle (upstream
    // `perform_legacy_request` used `aesgcm`, which this server does not
    // implement — the tickle stays RFC 8030-compliant and is documented in
    // the D42 register note).
    let request = if subscription.standard {
        let payload_json = push_notification_payload(store, &notification)?;
        match build_push_request_encrypted(&subscription, keys, subject, now_ms, &payload_json) {
            Ok(request) => request,
            // Unusable keys (corrupt or undecodable rows that passed the
            // write-time validation): drop the subscription, like upstream's
            // invalid-row destroy. Never retries: the keys will not heal.
            // The reason names only roles and lengths, never key bytes.
            Err(message) => {
                tracing::warn!(reason = message, "web_push_delivery: dropping subscription");
                return Ok(PushJobOutcome::DeleteSubscriptionAndComplete(subscription.id));
            }
        }
    } else {
        build_push_request(&subscription, keys, subject, now_ms).map_err(Error::Internal)?
    };
    match deliver_push(client, &request).await {
        PushOutcome::Delivered => Ok(PushJobOutcome::Complete),
        PushOutcome::Gone => Ok(PushJobOutcome::DeleteSubscriptionAndComplete(
            subscription.id,
        )),
        PushOutcome::Retry(message) => Ok(PushJobOutcome::Retry(message)),
    }
}

/// Whether the notification's activity still exists (upstream
/// `notification.activity.present?`).
fn notification_activity_present(
    store: &dyn crate::application::ports::Store,
    notification: &Notification,
) -> std::result::Result<bool, Error> {
    match notification.status_id {
        Some(status_id) => Ok(store
            .status_by_id(status_id)?
            .is_some_and(|status| !status.is_deleted())),
        None => Ok(notification.actor_id.is_some()),
    }
}

/// Worker entry point for a `WebPushDelivery` job record: parses
/// `{subscription_id, notification_id}` and runs [`handle_push_job`],
/// mapping the outcome onto the worker's settle contract.
///
/// Returns `Ok(())` on done (delivered, skipped, or subscription removed),
/// `Err((false, message))` to retry with backoff, `Err((true, message))` to
/// quarantine (malformed payload, or the job's retry budget is spent by the
/// caller).
pub async fn run_push_job_record(
    store: &dyn crate::application::ports::Store,
    client: &reqwest::Client,
    keys: &VapidKeys,
    subject: &str,
    job: &crate::domain::Job,
    now_ms: u64,
) -> std::result::Result<(), (bool, String)> {
    let sub_id = job
        .payload
        .get("subscription_id")
        .and_then(|value| value.as_u64())
        .map(Id::new);
    let notification_id = job
        .payload
        .get("notification_id")
        .and_then(|value| value.as_u64())
        .map(Id::new);
    let (Some(sub_id), Some(notification_id)) = (sub_id, notification_id) else {
        return Err((
            true,
            "web_push_delivery: missing subscription_id/notification_id".to_string(),
        ));
    };
    let outcome = handle_push_job(store, client, keys, subject, sub_id, notification_id, now_ms)
        .await
        .map_err(|err| (false, format!("web_push_delivery: {err}")))?;
    match outcome {
        PushJobOutcome::Complete => Ok(()),
        PushJobOutcome::DeleteSubscriptionAndComplete(id) => {
            store
                .delete_push_subscription_by_id(id)
                .map_err(|err| (false, format!("web_push_delivery cleanup: {err}")))?;
            Ok(())
        }
        PushJobOutcome::Retry(message) => Err((false, message)),
    }
}

/// Serialize a subscription (`REST::WebPushSubscriptionSerializer`:
/// `{id, endpoint, standard, alerts, server_key, policy}`).
///
/// `alerts` carries every alert type this server produces, coerced to bools
/// (upstream casts stored values the same way); unknown stored keys are
/// dropped. `server_key` is the configured VAPID public key.
pub fn subscription_json(
    subscription: &WebPushSubscription,
    server_key: &str,
) -> serde_json::Value {
    let mut alerts = serde_json::Map::new();
    for known in crate::application::conversations::known_alert_types() {
        alerts.insert(
            (*known).to_string(),
            serde_json::Value::Bool(subscription.alerts.get(*known).copied().unwrap_or(false)),
        );
    }
    serde_json::json!({
        "id": subscription.id.to_string(),
        "endpoint": subscription.endpoint,
        "standard": subscription.standard,
        "alerts": alerts,
        "server_key": server_key,
        "policy": subscription.policy,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a fresh VAPID pair for tests by generating with `ring` and
    /// extracting the scalar and point from the PKCS#8 document (the
    /// `ECPrivateKey` OCTET STRING `04 20` and the public BIT STRING
    /// `03 42 00 04`, both unique markers in a generated P-256 document).
    fn test_keypair() -> VapidKeys {
        use base64::Engine as _;
        let rng = ring::rand::SystemRandom::new();
        let doc = ring::signature::EcdsaKeyPair::generate_pkcs8(
            &ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING,
            &rng,
        )
        .expect("generate");
        let der = doc.as_ref();
        let priv_off = der
            .windows(2)
            .position(|w| *w == [0x04, 0x20])
            .expect("priv marker");
        let priv_raw = &der[priv_off + 2..priv_off + 34];
        let pub_off = der
            .windows(4)
            .position(|w| *w == [0x03, 0x42, 0x00, 0x04])
            .expect("pub marker");
        let pub_raw = &der[pub_off + 3..pub_off + 3 + 65];
        assert_eq!(pub_raw[0], 0x04);
        let enc = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        VapidKeys {
            public_b64: enc.encode(pub_raw),
            private_b64: enc.encode(priv_raw),
        }
    }

    #[test]
    fn audience_origin_drops_path_and_default_port() {
        assert_eq!(
            push_audience_origin("https://push.example/sub/123?x=1").unwrap(),
            "https://push.example"
        );
        assert_eq!(
            push_audience_origin("http://push.example:8080/sub").unwrap(),
            "http://push.example:8080"
        );
        assert!(push_audience_origin("not a url").is_err());
    }

    #[test]
    fn response_classification() {
        assert_eq!(classify_push_response(201), PushOutcome::Delivered);
        assert_eq!(classify_push_response(404), PushOutcome::Gone);
        assert_eq!(classify_push_response(410), PushOutcome::Gone);
        assert!(matches!(
            classify_push_response(429),
            PushOutcome::Retry(_)
        ));
        assert!(matches!(
            classify_push_response(500),
            PushOutcome::Retry(_)
        ));
    }

    #[test]
    fn vapid_keys_configuration_gate() {
        assert!(!VapidKeys {
            public_b64: String::new(),
            private_b64: String::new(),
        }
        .is_configured());
        let keys = test_keypair();
        assert!(keys.is_configured());
    }

    #[test]
    fn vapid_jwt_builds_and_verifies() {
        use base64::Engine as _;
        let keys = test_keypair();
        let jwt = vapid_jwt(&keys, "https://push.example", "mailto:admin@example.social", 1_700_000_000_000)
            .expect("jwt");
        let mut parts = jwt.split('.');
        let (header_b64, payload_b64, sig_b64) = (
            parts.next().expect("header"),
            parts.next().expect("payload"),
            parts.next().expect("signature"),
        );
        assert!(parts.next().is_none());
        let enc = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let header: serde_json::Value =
            serde_json::from_slice(&enc.decode(header_b64).expect("header b64")).expect("header json");
        assert_eq!(header, serde_json::json!({"typ": "JWT", "alg": "ES256"}));
        let payload: serde_json::Value =
            serde_json::from_slice(&enc.decode(payload_b64).expect("payload b64")).expect("payload json");
        assert_eq!(payload["aud"], serde_json::json!("https://push.example"));
        assert_eq!(payload["sub"], serde_json::json!("mailto:admin@example.social"));
        assert_eq!(
            payload["exp"],
            serde_json::json!(1_700_000_000u64 + VAPID_JWT_LIFETIME_SECS)
        );
        // The signature verifies against the configured public key.
        let public_raw = enc.decode(keys.public_b64.trim()).expect("pub");
        let verifier = ring::signature::UnparsedPublicKey::new(
            &ring::signature::ECDSA_P256_SHA256_FIXED,
            public_raw,
        );
        let message = format!("{header_b64}.{payload_b64}");
        verifier
            .verify(
                message.as_bytes(),
                &enc.decode(sig_b64).expect("sig b64"),
            )
            .expect("signature verifies");
    }

    #[test]
    fn vapid_jwt_rejects_bad_keys() {
        let mut keys = test_keypair();
        keys.private_b64 = "eA".to_string();
        assert!(vapid_jwt(&keys, "https://push.example", "mailto:a@b.c", 0).is_err());
    }
}
