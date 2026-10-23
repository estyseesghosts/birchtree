//! Pinetree interop: quote Creates must parse.
//!
//! Pinetree emits the canonical FEP-044f `quote` *plus* legacy
//! compatibility aliases (`quoteUri`, `_misskey_quote`, `quoteUrl`) in one
//! object. The aliases used to be serde `alias`es of a single field, so any
//! real quote failed with `duplicate field 'quote'` and the whole activity
//! 422'd. Each spelling now has its own slot; canonical wins.
use activitypub_federation::protocol::context::WithContext;
use birchtree::activitypub::activities::Activity;

const PINETREE_QUOTE_CREATE: &str = include_str!("fixtures/pinetree-quote-create.json");

#[test]
fn pinetree_quote_create_parses() {
    let act: WithContext<Activity> = serde_json::from_str(PINETREE_QUOTE_CREATE)
        .expect("pinetree quote Create must parse");
    match act.inner() {
        Activity::Create(c) => {
            assert_eq!(
                c.id.as_str(),
                "http://10.88.111.34:3002/notes/asp09f14746te4m5/activity"
            );
        }
        other => panic!("expected Create, got {other:?}"),
    }
}

#[test]
fn quote_aliases_merge_with_canonical_priority() {
    let target = "http://10.88.111.34:3000/users/demo03/117492082056036352";
    let doc: serde_json::Value = serde_json::from_str(PINETREE_QUOTE_CREATE).unwrap();
    let note: birchtree::activitypub::wire::Note =
        serde_json::from_value(doc.get("object").unwrap().clone()).unwrap();
    // All four spellings present; canonical wins.
    assert_eq!(
        note.quote.as_ref().map(|u| u.as_str()),
        Some(target)
    );
    assert_eq!(note.quote_uri().map(|u| u.as_str()), Some(target));
}
