//! Follow-suggestion ranking and profile-directory listing (fix round P1/S1 + P3).
//!
//! Upstream (`app/models/account_suggestions.rb`,
//! `app/controllers/api/v1/suggestions_controller.rb`,
//! `app/controllers/api/v2/suggestions_controller.rb` and
//! `app/controllers/api/v1/directories_controller.rb` at pin `8ff11c2b`)
//! serves real suggestion objects and a real profile directory; `DELETE`
//! answers `200 {}` via `render_empty`
//! (`app/controllers/api/base_controller.rb:86-88`).
//!
//! Ranking source (honest, local-only). Upstream ranks from five sources
//! (`account_suggestions.rb:4-10`: staff picks, friends-of-friends,
//! similar-profiles, a locale `FollowRecommendation` table, FASP) backed by
//! interaction history this build does not store. Fabricating those signals
//! would be a success-shaped stub, so this engine ranks only what the local
//! store can prove:
//!
//! * candidates: LOCAL, non-suspended, non-silenced accounts with no
//!   `moved_to` (mirrors `source.rb base_account_scope` minus the stores we
//!   lack: `discoverable` flag, memorial flag, `follow_recommendation_mutes`);
//! * friends-of-friends: accounts followed by the viewer's followees, ordered
//!   by mutual count desc then followers asc (mirrors
//!   `friends_of_friends_source.rb` SQL `ORDER BY frequency DESC,
//!   followers_count ASC`);
//! * global fill: remaining candidates ordered by followers desc, then recent
//!   activity (`last_status_at` desc, nulls last — mirrors
//!   `account_stat.rb:22 by_recent_status`), then id asc for determinism
//!   (upstream shuffles the merged cache instead; determinism here is ours);
//! * exclusions mirror `source.rb base_account_scope`: self, already-followed,
//!   pending follow requests, either-direction blocks, mutes,
//!   domain-blocks (`relationships` aggregate + `account.rb:467-473`) and
//!   persisted dismissals (`follow_recommendation_mutes`).
//!
//! Limits (stated, not hidden): remote accounts are never suggested (no
//! honest remote ranking signal); staff picks / similar-profiles /
//! locale-recommendation / FASP sources do not exist here; dismissals persist
//! in `SUGGESTION_MUTES` (schema v13) and rejoin the exclusion set. Candidate
//! enumeration is a bounded handle-index scan (`CANDIDATE_SCAN_CAP`), so on
//! instances larger than the cap the tail is never suggested.

use std::collections::{HashMap, HashSet};

use crate::application::ports::Store;
use crate::domain::{Account, AccountStats, Id, Result};

/// Upstream `DEFAULT_ACCOUNTS_LIMIT` (`api/base_controller.rb:5`).
pub const DEFAULT_LIMIT: usize = 40;
/// Upstream `limit_param` caps at twice the default (`api/base_controller.rb:33-37`).
pub const MAX_LIMIT: usize = 80;
/// Bounded handle-index scan for candidate enumeration (see module docs).
const CANDIDATE_SCAN_CAP: usize = 500;
/// Bound on the viewer's own follow list read (exclusion set + FoF seeds).
const FOLLOW_SCAN_CAP: usize = 500;
/// Bound on friends-of-friends seed accounts.
const FOF_SEED_CAP: usize = 50;
/// Bound on each seed's follow list read.
const FOF_PER_SEED_CAP: usize = 100;
/// Bound on the viewer's dismissal list read (exclusion set).
const DISMISS_SCAN_CAP: usize = 500;

/// The v2 `sources` entry for a suggestion. `friends_of_friends` mirrors
/// `friends_of_friends_source.rb`; `global` stands in for the locale
/// `FollowRecommendation` table (`global_source.rb`) with an honest local
/// most-followed signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestionSource {
    FriendsOfFriends,
    Global,
}

impl SuggestionSource {
    /// The v2 `sources[]` entry.
    pub fn as_str(self) -> &'static str {
        match self {
            SuggestionSource::FriendsOfFriends => "friends_of_friends",
            SuggestionSource::Global => "global",
        }
    }

    /// The v2 legacy `source` (`rest/suggestion_serializer.rb:8-15`
    /// `LEGACY_SOURCE_TYPE_MAP`: `friends_of_friends` → `past_interactions`,
    /// `most_followed` → `global`).
    pub fn legacy(self) -> &'static str {
        match self {
            SuggestionSource::FriendsOfFriends => "past_interactions",
            SuggestionSource::Global => "global",
        }
    }
}

/// One ranked suggestion: the account, its counters, and its v2 source.
pub struct RankedSuggestion {
    pub account: Account,
    pub stats: Option<AccountStats>,
    pub source: SuggestionSource,
}

/// Mirror of upstream `limit_param(DEFAULT_ACCOUNTS_LIMIT)`
/// (`api/base_controller.rb:33-37`): absent → 40, else `min(abs, 80)`
/// (upstream `?limit=0` yields an empty page; `offset` clamps at 0).
pub fn parse_limit_offset(limit_raw: Option<&str>, offset_raw: Option<&str>) -> (usize, usize) {
    let limit = match limit_raw {
        None => DEFAULT_LIMIT,
        Some(raw) => {
            let n: i64 = raw.parse().unwrap_or(0);
            (n.unsigned_abs() as usize).min(MAX_LIMIT)
        }
    };
    let offset = match offset_raw {
        None => 0,
        Some(raw) => raw.parse::<i64>().map(|n| n.max(0) as usize).unwrap_or(0),
    };
    (limit, offset)
}

/// Rank follow suggestions for `viewer` (v1 + v2 share the ranking; only the
/// serializer differs, per the two upstream controllers).
pub fn rank_suggestions(
    store: &dyn Store,
    viewer: Id,
    limit: usize,
    offset: usize,
    now_ms: u64,
) -> Result<Vec<RankedSuggestion>> {
    let limit = limit.min(MAX_LIMIT);
    if limit == 0 {
        return Ok(Vec::new());
    }

    let (following_set, dismissed, frequency) = load_follow_graph(store, viewer)?;
    let mut accounts = load_candidate_pool(store, &frequency)?;
    accounts.retain(|id, account| {
        *id != viewer
            && !following_set.contains(id)
            && !dismissed.contains(id)
            && account.local
            && !account.suspended
            && !account.silenced
            && account.moved_to.is_none()
    });
    apply_pairwise_exclusions(store, viewer, now_ms, &mut accounts)?;
    let ids: Vec<Id> = accounts.keys().copied().collect();
    let stats = store.account_stats_by_ids(&ids)?;
    let (fof, global) = split_and_sort(&frequency, accounts, &stats);
    Ok(fof
        .into_iter()
        .chain(global)
        .skip(offset)
        .take(limit)
        .collect())
}

/// The viewer's follow graph: exclusion set, dismissed set, and
/// friends-of-friends frequencies over a bounded seed window.
fn load_follow_graph(
    store: &dyn Store,
    viewer: Id,
) -> Result<(HashSet<Id>, HashSet<Id>, HashMap<Id, usize>)> {
    // The viewer's follow graph: exclusion set and friends-of-friends seeds.
    let following = store.following(viewer, None, FOLLOW_SCAN_CAP)?;
    let following_set: HashSet<Id> = following.iter().copied().collect();

    // Dismissed suggestions rejoin the exclusion set (upstream
    // `follow_recommendation_mutes`; durable since schema v13).
    let dismissed: HashSet<Id> = store
        .dismissed_suggestions(viewer, DISMISS_SCAN_CAP)
        .unwrap_or_default()
        .into_iter()
        .collect();

    // Friends-of-friends frequencies over a bounded seed window.
    let mut frequency: HashMap<Id, usize> = HashMap::new();
    for seed in following.iter().take(FOF_SEED_CAP) {
        if *seed == viewer {
            continue;
        }
        for candidate in store.following(*seed, None, FOF_PER_SEED_CAP)? {
            if candidate != viewer {
                *frequency.entry(candidate).or_insert(0) += 1;
            }
        }
    }
    Ok((following_set, dismissed, frequency))
}

/// Candidate pool: bounded handle-index scan (`search_accounts` with an
/// empty needle matches every handle) plus any FoF-only ids.
fn load_candidate_pool(
    store: &dyn Store,
    frequency: &HashMap<Id, usize>,
) -> Result<HashMap<Id, Account>> {
    let scanned = store.search_accounts("", CANDIDATE_SCAN_CAP)?;
    let mut accounts: HashMap<Id, Account> =
        scanned.into_iter().map(|account| (account.id, account)).collect();
    let missing: Vec<Id> = frequency
        .keys()
        .filter(|id| !accounts.contains_key(id))
        .copied()
        .collect();
    if !missing.is_empty() {
        for (id, account) in store.accounts_by_ids(&missing)? {
            accounts.insert(id, account);
        }
    }
    Ok(accounts)
}

/// Pairwise exclusions in ONE batch (mirrors `not_excluded_by_account` +
/// `not_domain_blocked_by_account`, `account.rb:164-165,467-473`: either
/// direction blocks, mutes, domain blocks, plus pending outgoing requests).
fn apply_pairwise_exclusions(
    store: &dyn Store,
    viewer: Id,
    now_ms: u64,
    accounts: &mut HashMap<Id, Account>,
) -> Result<()> {
    let targets: Vec<(Id, String)> = accounts
        .values()
        .map(|account| (account.id, account.domain.clone()))
        .collect();
    if !targets.is_empty() {
        let excluded: HashSet<Id> = store
            .relationships(viewer, &targets, now_ms)?
            .into_iter()
            .filter(|rel| {
                rel.following
                    || rel.requested
                    || rel.blocking
                    || rel.blocked_by
                    || rel.muting
                    || rel.domain_blocking
            })
            .map(|rel| rel.target_id)
            .collect();
        accounts.retain(|id, _| !excluded.contains(id));
    }
    Ok(())
}

/// Friends-of-friends first (frequency desc, followers asc — mirrors the
/// FoF SQL — then id asc), then the global most-followed fill (followers
/// desc, recent activity desc nulls-last, id asc).
fn split_and_sort(
    frequency: &HashMap<Id, usize>,
    accounts: HashMap<Id, Account>,
    stats: &HashMap<Id, AccountStats>,
) -> (Vec<RankedSuggestion>, Vec<RankedSuggestion>) {
    let mut fof: Vec<RankedSuggestion> = Vec::new();
    let mut global: Vec<RankedSuggestion> = Vec::new();
    for (id, account) in accounts {
        let entry = RankedSuggestion {
            stats: stats.get(&id).copied(),
            account,
            source: if frequency.contains_key(&id) {
                SuggestionSource::FriendsOfFriends
            } else {
                SuggestionSource::Global
            },
        };
        if frequency.contains_key(&id) {
            fof.push(entry);
        } else {
            global.push(entry);
        }
    }
    fof.sort_by(|a, b| {
        let fa = frequency.get(&a.account.id).copied().unwrap_or(0);
        let fb = frequency.get(&b.account.id).copied().unwrap_or(0);
        fb.cmp(&fa)
            .then_with(|| followers(&a.stats).cmp(&followers(&b.stats)))
            .then_with(|| a.account.id.value().cmp(&b.account.id.value()))
    });
    global.sort_by(|a, b| {
        followers(&b.stats)
            .cmp(&followers(&a.stats))
            .then_with(|| last_status(&b.stats).cmp(&last_status(&a.stats)))
            .then_with(|| a.account.id.value().cmp(&b.account.id.value()))
    });
    (fof, global)
}

fn followers(stats: &Option<AccountStats>) -> u64 {
    stats.map(|s| s.followers_count).unwrap_or(0)
}

fn last_status(stats: &Option<AccountStats>) -> u64 {
    stats.and_then(|s| s.last_status_at_ms).unwrap_or(0)
}

/// Profile-directory query (`directories_controller.rb:33-44`: `local`,
/// `order` in `new|active`, `offset`, `limit`).
pub struct DirectoryQuery {
    pub local_only: bool,
    pub order_new: bool,
    pub limit: usize,
    pub offset: usize,
}

/// List directory entries: non-suspended, non-silenced, non-moved accounts
/// (mirrors `Account.discoverable` = `searchable.without_silenced.where
/// discoverable` for the columns this build stores; the `discoverable` flag
/// itself has no store yet — stated limit). Exclusions mirror the controller
/// exactly (`directories_controller.rb:24-31`): either-direction blocks and
/// mutes always, domain-blocks only when remote accounts are included.
/// Followed accounts are NOT excluded (unlike suggestions).
pub fn directory_accounts(
    store: &dyn Store,
    viewer: Option<Id>,
    query: &DirectoryQuery,
    now_ms: u64,
) -> Result<Vec<(Account, Option<AccountStats>)>> {
    let limit = query.limit.min(MAX_LIMIT);
    if limit == 0 {
        return Ok(Vec::new());
    }
    // The scan order (handle-lexicographic) is not the display order, so
    // over-fetch a bounded headroom for filtering/sorting before paging.
    // Exact within the cap; approximate beyond it (stated limit).
    let scan_cap = query
        .offset
        .saturating_add(limit)
        .saturating_add(200)
        .min(CANDIDATE_SCAN_CAP);
    let scanned = store.search_accounts("", scan_cap.max(limit))?;

    let mut accounts: Vec<Account> = scanned
        .into_iter()
        .filter(|account| {
            !account.suspended && !account.silenced && account.moved_to.is_none()
        })
        .filter(|account| !query.local_only || account.local)
        .collect();

    if let Some(viewer) = viewer {
        let targets: Vec<(Id, String)> = accounts
            .iter()
            .filter(|account| account.id != viewer)
            .map(|account| (account.id, account.domain.clone()))
            .collect();
        if !targets.is_empty() {
            let excluded: HashSet<Id> = store
                .relationships(viewer, &targets, now_ms)?
                .into_iter()
                .filter(|rel| {
                    rel.blocking
                        || rel.blocked_by
                        || rel.muting
                        || (!query.local_only && rel.domain_blocking)
                })
                .map(|rel| rel.target_id)
                .collect();
            accounts.retain(|account| account.id == viewer || !excluded.contains(&account.id));
        }
    }

    let ids: Vec<Id> = accounts.iter().map(|account| account.id).collect();
    let stats = store.account_stats_by_ids(&ids)?;

    if query.order_new {
        // Mirrors `Account.order(id: :desc)`.
        accounts.sort_by(|a, b| b.id.value().cmp(&a.id.value()));
    } else {
        // Mirrors `Account.by_recent_status`
        // (`account_stat.rb:22`: `last_status_at desc nulls_last`); id desc
        // tiebreak is ours (determinism).
        accounts.sort_by(|a, b| {
            let sa = stats.get(&a.id).and_then(|s| s.last_status_at_ms);
            let sb = stats.get(&b.id).and_then(|s| s.last_status_at_ms);
            sb.cmp(&sa)
                .then_with(|| b.id.value().cmp(&a.id.value()))
        });
    }

    Ok(accounts
        .into_iter()
        .skip(query.offset)
        .take(limit)
        .map(|account| {
            let entry_stats = stats.get(&account.id).copied();
            (account, entry_stats)
        })
        .collect())
}
