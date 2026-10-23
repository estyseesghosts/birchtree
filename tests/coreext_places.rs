//! CORE-P (places stub) on `feature/core-extensions`: domain validation,
//! the pure policy gate, the registry, and the HTTP matrix
//! (off/unenforced/enforced x create/edit + 422 + wire round-trip) plus
//! ActivityPub emit/ingest goldens.
//!
//! Policy summary (`docs/configuration.md`): `off` = disabled (`place_id`
//! accepted-and-ignored, stored NULL, wire omits `location`);
//! `unenforced` = enabled, optional (unknown `place_id` → existing 422
//! path; missing allowed); `enforced` = enabled, mandatory (top-level
//! non-DM statuses require a resolvable `place_id`, else 422; replies and
//! DMs exempt). No proximity/discover/geo-search.

mod common;

use axum::http::StatusCode;
use serde_json::Value;

use birchtree::application::place_policy::check_place_policy;
use birchtree::application::ports::PlaceStore;
use birchtree::domain::place::{Place, validate_place_coords, validate_place_name};
use birchtree::domain::{Error, Id};

use common::{TestServer, form_request, get_request, json, send};

const ALL_SCOPES: &str = "read write follow read:accounts write:accounts read:statuses \
                         write:statuses write:media";

// ---------------------------------------------------------------------------
// Domain validation.
// ---------------------------------------------------------------------------

#[test]
fn place_name_rules() {
    assert!(validate_place_name("Berlin").is_ok());
    assert!(matches!(
        validate_place_name("   "),
        Err(Error::InvalidInput(_))
    ));
    assert!(matches!(validate_place_name(""), Err(Error::InvalidInput(_))));
    let long: String = std::iter::repeat_n('x', 257).collect();
    assert!(matches!(
        validate_place_name(&long),
        Err(Error::InvalidInput(_))
    ));
}

#[test]
fn place_coord_rules() {
    assert!(validate_place_coords(13.4, 52.5).is_ok());
    assert!(validate_place_coords(-180.0, -90.0).is_ok());
    assert!(validate_place_coords(180.0, 90.0).is_ok());
    assert!(matches!(
        validate_place_coords(180.1, 0.0),
        Err(Error::InvalidInput(_))
    ));
    assert!(matches!(
        validate_place_coords(0.0, -90.1),
        Err(Error::InvalidInput(_))
    ));
    assert!(matches!(
        validate_place_coords(f64::NAN, 0.0),
        Err(Error::InvalidInput(_))
    ));
    assert!(matches!(
        validate_place_coords(0.0, f64::INFINITY),
        Err(Error::InvalidInput(_))
    ));
}

#[test]
fn place_json_round_trip() {
    let place = Place {
        id: Id::new(7),
        name: "Berlin".into(),
        lon: 13.4,
        lat: 52.5,
        country: Some("DE".into()),
    };
    let bytes = serde_json::to_vec(&place).expect("encode");
    let back: Place = serde_json::from_slice(&bytes).expect("decode");
    assert_eq!(back, place);
}

// ---------------------------------------------------------------------------
// Pure policy matrix.
// ---------------------------------------------------------------------------

fn some_id() -> Option<Id> {
    Some(Id::new(123_456_789))
}

#[test]
fn off_always_accepted_and_ignored() {
    for visibility_is_direct in [false, true] {
        for is_reply in [false, true] {
            for (place_id, resolves) in
                [(None, false), (None, true), (some_id(), false), (some_id(), true)]
            {
                let got =
                    check_place_policy("off", visibility_is_direct, is_reply, place_id, resolves)
                        .expect("off must never fail");
                assert_eq!(got, None, "off must store NULL");
            }
        }
    }
}

#[test]
fn unenforced_optional_but_unknown_rejected() {
    for visibility_is_direct in [false, true] {
        for is_reply in [false, true] {
            assert_eq!(
                check_place_policy("unenforced", visibility_is_direct, is_reply, None, false)
                    .unwrap(),
                None
            );
        }
    }
    assert_eq!(
        check_place_policy("unenforced", false, false, some_id(), true).unwrap(),
        some_id()
    );
    for visibility_is_direct in [false, true] {
        for is_reply in [false, true] {
            let err = check_place_policy(
                "unenforced",
                visibility_is_direct,
                is_reply,
                some_id(),
                false,
            )
            .expect_err("unknown place_id must fail");
            assert!(matches!(err, Error::InvalidInput(_)), "got {err:?}");
        }
    }
}

#[test]
fn enforced_requires_place_on_top_level_non_dm() {
    let err =
        check_place_policy("enforced", false, false, None, false).expect_err("missing must fail");
    assert!(matches!(err, Error::InvalidInput(_)), "got {err:?}");
    let err =
        check_place_policy("enforced", false, false, some_id(), false).expect_err("unknown fail");
    assert!(matches!(err, Error::InvalidInput(_)), "got {err:?}");
    assert_eq!(
        check_place_policy("enforced", false, false, some_id(), true).unwrap(),
        some_id()
    );
}

#[test]
fn enforced_exempts_replies_and_dms() {
    assert_eq!(
        check_place_policy("enforced", false, true, None, false).unwrap(),
        None
    );
    assert_eq!(
        check_place_policy("enforced", true, false, None, false).unwrap(),
        None
    );
    assert!(matches!(
        check_place_policy("enforced", false, true, some_id(), false).unwrap_err(),
        Error::InvalidInput(_)
    ));
}

#[test]
fn unknown_policy_is_server_misconfiguration() {
    let err =
        check_place_policy("sometimes", false, false, None, false).expect_err("must fail");
    assert!(matches!(err, Error::Internal(_)), "got {err:?}");
}

// ---------------------------------------------------------------------------
// HTTP matrix.
// ---------------------------------------------------------------------------

fn enc(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

async fn register_app(srv: &TestServer, scopes: &str) -> Value {
    let body = format!(
        "client_name=Test+App&redirect_uris={}&scopes={}",
        enc("https://client.example/cb"),
        enc(scopes),
    );
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", "/api/v1/apps", &body, None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

async fn app_token(srv: &TestServer, app: &Value, scopes: &str) -> String {
    let body = format!(
        "grant_type=client_credentials&client_id={}&client_secret={}&scope={}",
        enc(app["client_id"].as_str().unwrap()),
        enc(app["client_secret"].as_str().unwrap()),
        enc(scopes),
    );
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", "/oauth/token", &body, None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)["access_token"].as_str().unwrap().to_string()
}

async fn register_account(srv: &TestServer, app: &Value, username: &str) -> String {
    let app = app_token(srv, app, "write:accounts").await;
    let body = format!(
        "username={}&email={}&password=password123&agreement=true&locale=en",
        enc(username),
        enc(&format!("{username}@example.social")),
    );
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", "/api/v1/accounts", &body, Some(&app))).await;
    assert_eq!(status, StatusCode::OK, "register: {}", String::from_utf8_lossy(&bytes));
    json(&bytes)["access_token"].as_str().unwrap().to_string()
}

fn server_with_policy(policy: &str) -> TestServer {
    TestServer::with_config(true, |cfg| {
        cfg.place_policy = policy.to_string();
    })
}

fn seed_place(srv: &TestServer, id: u64, name: &str) -> Id {
    let place = Place {
        id: Id::new(id),
        name: name.into(),
        lon: 13.4,
        lat: 52.5,
        country: Some("DE".into()),
    };
    srv.store.upsert_place(place).expect("seed place").id
}

async fn setup(policy: &str, username: &str) -> (TestServer, String) {
    let srv = server_with_policy(policy);
    let app = register_app(&srv, ALL_SCOPES).await;
    let token = register_account(&srv, &app, username).await;
    (srv, token)
}

async fn post_status(
    srv: &TestServer,
    token: &str,
    place_id: Option<&str>,
    visibility: Option<&str>,
    reply_to: Option<&str>,
) -> (StatusCode, Value) {
    let mut body = String::from("status=hello");
    if let Some(id) = place_id {
        body.push_str(&format!("&place_id={}", enc(id)));
    }
    if let Some(v) = visibility {
        body.push_str(&format!("&visibility={v}"));
    }
    if let Some(parent) = reply_to {
        body.push_str(&format!("&in_reply_to_id={}", enc(parent)));
    }
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", "/api/v1/statuses", &body, Some(token))).await;
    (status, json(&bytes))
}

#[tokio::test]
async fn off_ignores_place_id_and_omits_location() {
    let (srv, token) = setup("off", "placeoff").await;
    // Unknown id is accepted-and-ignored under `off`.
    let (status, created) = post_status(&srv, &token, Some("987654321"), None, None).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created.get("location"), None, "wire omits location when off");
    // Direct fetch agrees (stored NULL).
    let id = created["id"].as_str().unwrap().to_string();
    let (status, _h, bytes) = send(
        &srv.router,
        get_request(&format!("/api/v1/statuses/{id}"), Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes).get("location"), None);
}

#[tokio::test]
async fn unenforced_round_trips_and_422s_unknown() {
    let (srv, token) = setup("unenforced", "placeopen").await;
    let place = seed_place(&srv, 9001, "Berlin");
    // Missing place allowed.
    let (status, created) = post_status(&srv, &token, None, None, None).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created.get("location"), None);
    // Known place round-trips.
    let (status, created) =
        post_status(&srv, &token, Some(&place.to_string()), None, None).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let location = &created["location"];
    assert_eq!(location["name"], serde_json::json!("Berlin"));
    assert_eq!(location["longitude"], serde_json::json!(13.4));
    assert_eq!(location["latitude"], serde_json::json!(52.5));
    assert_eq!(location["country"], serde_json::json!("DE"));
    assert_eq!(location["id"], serde_json::json!(place.to_string()));
    // Unknown place_id → 422.
    let (status, body) = post_status(&srv, &token, Some("987654321"), None, None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
}

#[tokio::test]
async fn enforced_requires_place_with_exemptions() {
    let (srv, token) = setup("enforced", "placestrict").await;
    let place = seed_place(&srv, 9002, "Paris");
    // Top-level without place → 422.
    let (status, body) = post_status(&srv, &token, None, None, None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    // Top-level with unknown place → 422.
    let (status, body) = post_status(&srv, &token, Some("987654321"), None, None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    // Top-level with known place → 200.
    let (status, created) =
        post_status(&srv, &token, Some(&place.to_string()), None, None).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["location"]["name"], serde_json::json!("Paris"));
    // Reply exempt (no place).
    let parent_id = created["id"].as_str().unwrap().to_string();
    let (status, reply) = post_status(&srv, &token, None, None, Some(&parent_id)).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    // DM exempt (no place).
    let (status, dm) = post_status(&srv, &token, None, Some("direct"), None).await;
    assert_eq!(status, StatusCode::OK, "{dm}");
    // PUT edit attaching a place + PUT clearing it.
    let status_id = created["id"].as_str().unwrap().to_string();
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "PUT",
            &format!("/api/v1/statuses/{status_id}"),
            "place_id=",
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{}", String::from_utf8_lossy(&bytes));
    let other = seed_place(&srv, 9003, "Lyon");
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "PUT",
            &format!("/api/v1/statuses/{status_id}"),
            &format!("place_id={}", enc(&other.to_string())),
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes)["location"]["name"], serde_json::json!("Lyon"));
}

// ---------------------------------------------------------------------------
// ActivityPub goldens: emit gated on policy, ingest mints local rows.
// ---------------------------------------------------------------------------

#[test]
fn ap_emit_omits_location_when_off() {
    use birchtree::activitypub::outgoing;
    use birchtree::application::ports::{NewStatus, StatusStore};

    let (dir, store) = common::temp_store();
    let _dir = dir;
    let place = store
        .upsert_place(Place {
            id: Id::new(4242),
            name: "Berlin".into(),
            lon: 13.4,
            lat: 52.5,
            country: None,
        })
        .expect("seed");
    let status = store
        .create_status(
            NewStatus {
                account_id: common::mk_account(&store, "apoff").id,
                uri_base: "https://example.social/users/apoff/statuses".into(),
                url_base: "https://example.social/@apoff".into(),
                source_text: "hi".into(),
                content_html: "<p>hi</p>".into(),
                visibility: birchtree::domain::Visibility::Public,
                place_id: Some(place.id),
                ..NewStatus::default()
            },
            common::T0,
        )
        .expect("create")
        .status;
    let ctx = birchtree::activitypub::outgoing::OutgoingCtx {
        store: &*store,
        public_url: common::TEST_PUBLIC_URL,
        now_ms: common::T0,
        license_policy: "off",
        place_policy: "off",
    };
    let off = outgoing::note_for_status(&ctx, &status).expect("note");
    assert_eq!(off.get("location"), None);
    assert!(off["attachment"].as_array().unwrap().is_empty());
    let ctx = birchtree::activitypub::outgoing::OutgoingCtx {
        place_policy: "unenforced",
        license_policy: "unenforced",
        ..ctx
    };
    let on = outgoing::note_for_status(&ctx, &status).expect("note");
    let location = &on["location"];
    assert_eq!(location["name"], serde_json::json!("Berlin"));
    assert_eq!(location["type"], serde_json::json!("Place"));
}

#[test]
fn ap_ingest_mints_place_and_links_status() {
    use birchtree::activitypub::activities::note_input_for;
    use birchtree::activitypub::wire::Note;
    use birchtree::application::federation_inbound as inbound;

    fn test_url(raw: &str) -> url::Url {
        url::Url::parse(raw).unwrap()
    }

    let (dir, store) = common::temp_store();
    let _dir = dir;
    let author = common::mk_account(&store, "apin").id;
    let note = Note {
        kind: activitypub_federation::kinds::object::NoteType::Note,
        id: test_url("https://remote.example/notes/1"),
        attributed_to: test_url("https://remote.example/users/r"),
        content: "<p>hi</p>".into(),
        name: None,
        summary: None,
        sensitive: None,
        published: None,
        updated: None,
        in_reply_to: None,
        to: vec![test_url("https://www.w3.org/ns/activitystreams#Public")],
        cc: vec![],
        tag: vec![],
        attachment: vec![],
        quote: None,
        quote_authorization: None,
        content_map: None,
        url: None,
        replies: None,
        location: Some(birchtree::activitypub::wire::WirePlace {
            id: Some("99".into()),
            name: "Berlin".into(),
            longitude: 13.4,
            latitude: 52.5,
            country: Some("DE".into()),
        }),
    };
    let input = note_input_for(&note, author, false);
    assert_eq!(input.location.as_ref().expect("location parses").name, "Berlin");
    let ingested = inbound::ingest_note(&*store, input, None, common::T0).expect("ingest");
    let place_id = ingested.status.place_id.expect("status linked");
    // Sender id 99 is opaque: the stored row has a minted local id.
    assert_ne!(place_id.value(), 99);
    let stored = store.place_by_id(place_id).expect("row").expect("row");
    assert_eq!(stored.name, "Berlin");
    assert_eq!(stored.country.as_deref(), Some("DE"));
}
