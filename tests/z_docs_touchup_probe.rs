//! Docs/touch-up merge probe (`rebase/core-ext-onto-master`, §9).
//!
//! Ownership: this file is unique to the docs agent. It does NOT port
//! behavior; it pins the master-wins call sites that the 14 touched
//! suites must target after the parent merge:
//!
//! - `tests/common/mod.rs`: core-ext's `with_config` + `assemble_with_cfg`
//!   are added alongside master's `federation_config_with` (whose trailing
//!   `&[]` relays arg wins; the license/place policy params move onto the
//!   merged `build_config`, owned by the config agent).
//! - `note_input_for`: master's 3-arg arity `(note, author_id,
//!   author_sensitized)` wins; core-ext's extra `author_domain` is dropped.
//! - `AccountStore::update_account_profile`: master's
//!   `(account_id, update, now_ms)` wins; core-ext's 2-arg form is dropped.
//! - `federation_config_with`: `(context, url, http, debug)` is identical
//!   on both sides; only the `build_config` tail differs (internal to
//!   `tests/common/mod.rs`).
//!
//! Struct literals are deliberately avoided: the merge adds fields
//! (`wire::Note::location`, `RemoteNote::location`,
//! `RemoteAttachment::license`, `AccountProfileUpdate::default_license`),
//! so only function/type positions are pinned here.

mod common;

use std::sync::Arc;

use birchtree::activitypub::activities::note_input_for;
use birchtree::activitypub::wire::Note;
use birchtree::activitypub::FedContext;
use birchtree::application::context::Context;
use birchtree::application::federation_inbound::RemoteNote;
use birchtree::application::ports::{AccountProfileUpdate, AccountStore};
use birchtree::domain::Id;
use birchtree::federation::net::SafeHttpConfig;

use common::{TestServer, T0, mk_account};

/// Master's `note_input_for` arity wins (3 args; no `author_domain`).
/// Type-level pin: breaks if the merged signature drifts.
#[test]
fn docs_probe_note_input_for_arity_is_master_three_arg() {
    let f: fn(&Note, Id, bool) -> RemoteNote = note_input_for;
    let _ = f;
}

/// `federation_config_with` keeps the shared 4-arg shape on both sides.
/// Type-level pin: breaks if the merged helper drifts.
#[test]
fn docs_probe_federation_config_with_shape() {
    let f: fn(
        Arc<Context>,
        &str,
        &SafeHttpConfig,
        bool,
    ) -> activitypub_federation::config::FederationConfig<FedContext> =
        common::federation_config_with;
    let _ = f;
}

/// End to end through the merged `TestServer` path: server builds (which
/// exercises `federation_config_with` with master's trailing `&[]`), and
/// `update_account_profile` takes master's trailing `now_ms`.
#[test]
fn docs_probe_server_builds_and_profile_update_takes_now_ms() {
    let srv = TestServer::new(false);
    let alice = mk_account(&srv.store, "alice");
    let updated = srv
        .store
        .update_account_profile(
            alice.id,
            AccountProfileUpdate {
                hide_collections: Some(true),
                ..Default::default()
            },
            T0,
        )
        .expect("profile update with now_ms");
    assert_eq!(updated.hide_collections, Some(true));
}
