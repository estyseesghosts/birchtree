use crate::domain::Id;

/// Invite-token validity and code generation (round 3, area A).
///
/// Upstream: `app/models/invite.rb` at v4.7.3 (`8ff11c2b`).
/// `Invite#valid_for_use?` (`invite.rb:27-33`) is
/// `(max_uses.nil? || uses < max_uses) && !expired? && user&.functional?`.
/// The inviter-functional check (not suspended/disabled) lives at the
/// application layer, which can read the inviter's credential row; the
/// counter/expiry checks live here so both REST and any future ActivityPub
/// caller share them. Domain knows no HTTP or database shapes.

/// Length of an invite code (upstream `set_code`, `invite.rb:44-49`).
pub const INVITE_CODE_LEN: usize = 8;

/// Maximum invite comment length (upstream `COMMENT_SIZE_LIMIT`, `invite.rb:13`).
pub const INVITE_COMMENT_LIMIT: usize = 420;

/// Upstream `VALID_CODE_CHARACTERS` (`invite.rb:13-19`): alphanumerics minus
/// the homoglyphs `0 1 I l O` (56 characters).
pub const INVITE_CODE_ALPHABET: &[u8] =
    b"abcdefghijkmnopqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/// Derive an 8-character invite code from a 64-bit seed with xorshift64*.
///
/// Pure function (no RNG in the domain layer): the application layer feeds a
/// fresh seed per attempt and retries on code collision, so uniqueness comes
/// from the store, not from this mapping.
pub fn invite_code_from_seed(mut seed: u64) -> String {
    if seed == 0 {
        seed = 0x9E3779B97F4A7C15;
    }
    let mut out = String::with_capacity(INVITE_CODE_LEN);
    for _ in 0..INVITE_CODE_LEN {
        seed ^= seed >> 12;
        seed ^= seed << 25;
        seed ^= seed >> 27;
        let idx =
            (seed.wrapping_mul(0x2545F4914F6CDD1D) >> 33) as usize % INVITE_CODE_ALPHABET.len();
        out.push(INVITE_CODE_ALPHABET[idx] as char);
    }
    out
}

/// Whether `code` has the upstream invite-code shape (8 chars, alphabet-only).
pub fn is_valid_invite_code_shape(code: &str) -> bool {
    code.len() == INVITE_CODE_LEN && code.bytes().all(|b| INVITE_CODE_ALPHABET.contains(&b))
}

/// Why an invite token cannot be used (known code, unusable state).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InviteRefusal {
    /// Past `expires_at_ms` (upstream `Expireable#expired?`).
    Expired,
    /// `uses >= max_uses` (upstream `valid_for_use?` counter half).
    Exhausted,
}

/// An invite token (upstream `invites` table, `app/models/invite.rb:3-17`).
///
/// `autofollow` is intentionally absent: upstream's web flow follows the
/// inviter back on signup (`registrations_controller.rb: after_sign_in_path`),
/// which has no API-observable equivalent here; recording it would be a
/// success-shaped stub. `user_id` is stored as the creator's account id plus
/// a staff snapshot (staff-issued invites bypass the approval queue; see the
/// application layer).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Invite {
    pub id: Id,
    pub code: String,
    pub created_by_account_id: Id,
    /// Snapshot of whether the creator held a staff role when issuing.
    /// Staff-issued invites bypass `registration_requires_approval`
    /// (birchtree's analogue of upstream `bypass_approval?`, which keys off
    /// the `invite_bypass_approval` role permission we do not model).
    pub created_by_staff: bool,
    pub comment: Option<String>,
    /// Maximum redemptions (`None` = unlimited, upstream `max_uses` NULL).
    pub max_uses: Option<u32>,
    pub uses: u32,
    /// Expiry timestamp (`None` = never, upstream `expires_at` NULL).
    pub expires_at_ms: Option<u64>,
    pub created_at_ms: u64,
}

impl Invite {
    /// Upstream `Expireable#expired?`: expired only when an expiry is set and
    /// passed (a NULL `expires_at` is always available, `invite.rb:23` scope
    /// `available`).
    pub fn is_expired_at(&self, now_ms: u64) -> bool {
        self.expires_at_ms.is_some_and(|exp| now_ms >= exp)
    }

    /// The counter/expiry half of upstream `valid_for_use?` (`invite.rb:27-33`,
    /// minus the inviter-functional check done by the application layer).
    pub fn check_usable(&self, now_ms: u64) -> Result<(), InviteRefusal> {
        if self.is_expired_at(now_ms) {
            return Err(InviteRefusal::Expired);
        }
        if self.max_uses.is_some_and(|max| self.uses >= max) {
            return Err(InviteRefusal::Exhausted);
        }
        Ok(())
    }
}
