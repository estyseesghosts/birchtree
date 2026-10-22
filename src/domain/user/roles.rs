/// Staff role for the admin API (Phase 7.3).
///
/// Upstream models this as `UserRole` rows with a permission bitmask
/// (`app/models/user_role.rb`); birchtree collapses the role table to three
/// rows with synthetic numeric ids (`0` = everyone/`User`, `1` =
/// `Moderator`, `2` = `Admin`) carrying the upstream flag bits below. The
/// ids are stable and round-trip through the v2 `role_ids` filter; the
/// bitmask positions match upstream `UserRole::FLAGS` exactly (moderator =
/// `1049884`, verified against `user_role.rb:20-44` at `8ff11c2b`; an older
/// comment's `1049888` was an arithmetic slip).
/// Accounts without a role (`None`, the default for all pre-7.3 records) are
/// refused by the admin gate with `403 {"error":"Forbidden"}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UserRole {
    /// No staff role (default; every record written before roles existed).
    #[default]
    None,
    /// Moderation staff: exactly the upstream moderator flag set.
    Moderator,
    /// Full administrator: every upstream flag.
    Admin,
}

// Upstream `UserRole::FLAGS` bit positions (`app/models/user_role.rb:20-44`
// at `8ff11c2b`). Single canonical definition: the credential-account role
// serializer must import these (snippet in the G-E report) instead of
// keeping its own copy.
pub const PERM_VIEW_DASHBOARD: u64 = 1 << 3;
pub const PERM_VIEW_AUDIT_LOG: u64 = 1 << 2;
pub const PERM_VIEW_FEEDS: u64 = 1 << 20;
pub const PERM_MANAGE_USERS: u64 = 1 << 10;
pub const PERM_MANAGE_REPORTS: u64 = 1 << 4;
pub const PERM_MANAGE_TAXONOMIES: u64 = 1 << 8;
pub const PERM_MANAGE_FEDERATION: u64 = 1 << 5;
pub const PERM_MANAGE_SETTINGS: u64 = 1 << 6;
pub const PERM_MANAGE_BLOCKS: u64 = 1 << 7;
pub const PERM_MANAGE_APPEALS: u64 = 1 << 9;
pub const PERM_MANAGE_INVITES: u64 = 1 << 11;
pub const PERM_MANAGE_RULES: u64 = 1 << 12;
pub const PERM_MANAGE_ANNOUNCEMENTS: u64 = 1 << 13;
pub const PERM_MANAGE_CUSTOM_EMOJIS: u64 = 1 << 14;
pub const PERM_MANAGE_WEBHOOKS: u64 = 1 << 15;
pub const PERM_MANAGE_USER_ACCESS: u64 = 1 << 18;
pub const PERM_DELETE_USER_DATA: u64 = 1 << 19;
pub const PERM_MANAGE_ROLES: u64 = 1 << 17;

/// Upstream moderator flag set (`view_dashboard | view_audit_log |
/// view_feeds | manage_users | manage_reports | manage_taxonomies` =
/// `1049884`).
pub const MODERATOR_PERMISSIONS: u64 = PERM_VIEW_DASHBOARD
    | PERM_VIEW_AUDIT_LOG
    | PERM_VIEW_FEEDS
    | PERM_MANAGE_USERS
    | PERM_MANAGE_REPORTS
    | PERM_MANAGE_TAXONOMIES;

/// Upstream administrator flag set (moderator set plus every other flag).
pub const ADMIN_PERMISSIONS: u64 = MODERATOR_PERMISSIONS
    | PERM_MANAGE_FEDERATION
    | PERM_MANAGE_SETTINGS
    | PERM_MANAGE_BLOCKS
    | PERM_MANAGE_APPEALS
    | PERM_MANAGE_INVITES
    | PERM_MANAGE_RULES
    | PERM_MANAGE_ANNOUNCEMENTS
    | PERM_MANAGE_CUSTOM_EMOJIS
    | PERM_MANAGE_WEBHOOKS
    | PERM_MANAGE_USER_ACCESS
    | PERM_DELETE_USER_DATA
    | PERM_MANAGE_ROLES;

/// One fine-grained admin action, in the shape of upstream's
/// `Admin::BasePolicy` per-action checks: each action names the single
/// upstream permission flag the corresponding policy consults. The endpoint
/// gate (`require_admin_action`) refuses before the scope check, exactly
/// like upstream's `PermissionsConcern` â†’ controller `authorize` order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminAction {
    /// Read the admin account index/show (`manage_users`).
    ViewAccounts,
    /// Any account-moderation write: enable/disable/silence/suspend/
    /// approve/reject/destroy/strike (`manage_users`).
    ModerateAccount,
    /// Read and resolve the moderator report queue (`manage_reports`).
    ReviewReports,
    /// Domain allow/block management (`manage_federation`).
    ManageDomainBlocks,
    /// IP block management (`manage_blocks`).
    ManageIpBlocks,
    /// Email-domain and canonical-email block management (`manage_blocks`).
    ManageEmailBlocks,
    /// Tag review flags (`manage_taxonomies`).
    ManageTags,
    /// Measures/dimensions/retention reads (`view_dashboard`).
    ViewMetrics,
    /// Announcement authoring (`manage_announcements`).
    ManageAnnouncements,
}

impl AdminAction {
    /// The upstream permission flag this action requires.
    pub fn required_permission(self) -> u64 {
        match self {
            AdminAction::ViewAccounts | AdminAction::ModerateAccount => PERM_MANAGE_USERS,
            AdminAction::ReviewReports => PERM_MANAGE_REPORTS,
            AdminAction::ManageDomainBlocks => PERM_MANAGE_FEDERATION,
            AdminAction::ManageIpBlocks | AdminAction::ManageEmailBlocks => PERM_MANAGE_BLOCKS,
            AdminAction::ManageTags => PERM_MANAGE_TAXONOMIES,
            AdminAction::ViewMetrics => PERM_VIEW_DASHBOARD,
            AdminAction::ManageAnnouncements => PERM_MANAGE_ANNOUNCEMENTS,
        }
    }

    /// Stable name for audit rows and tests.
    pub fn name(self) -> &'static str {
        match self {
            AdminAction::ViewAccounts => "view_accounts",
            AdminAction::ModerateAccount => "moderate_account",
            AdminAction::ReviewReports => "review_reports",
            AdminAction::ManageDomainBlocks => "manage_domain_blocks",
            AdminAction::ManageIpBlocks => "manage_ip_blocks",
            AdminAction::ManageEmailBlocks => "manage_email_blocks",
            AdminAction::ManageTags => "manage_tags",
            AdminAction::ViewMetrics => "view_metrics",
            AdminAction::ManageAnnouncements => "manage_announcements",
        }
    }
}

impl UserRole {
    /// True for either staff role.
    pub fn is_staff(&self) -> bool {
        !matches!(self, UserRole::None)
    }

    /// Stable numeric id for the role serializer and the v2 `role_ids`
    /// filter (`0` = everyone/`User`, `1` = `Moderator`, `2` = `Admin`).
    /// Never `-1`: every id round-trips through [`UserRole::from_numeric_id`].
    pub fn numeric_id(self) -> u64 {
        match self {
            UserRole::None => 0,
            UserRole::Moderator => 1,
            UserRole::Admin => 2,
        }
    }

    /// Inverse of [`UserRole::numeric_id`]; `None` for unknown ids.
    pub fn from_numeric_id(id: u64) -> Option<UserRole> {
        match id {
            0 => Some(UserRole::None),
            1 => Some(UserRole::Moderator),
            2 => Some(UserRole::Admin),
            _ => None,
        }
    }

    /// Display name matching the seeded upstream role names.
    pub fn role_name(self) -> &'static str {
        match self {
            UserRole::None => "User",
            UserRole::Moderator => "Moderator",
            UserRole::Admin => "Admin",
        }
    }

    /// The upstream permission bitmask this role carries (`0` for
    /// non-staff, `1049888` for moderators, the full set for admins).
    pub fn permissions(self) -> u64 {
        match self {
            UserRole::None => 0,
            UserRole::Moderator => MODERATOR_PERMISSIONS,
            UserRole::Admin => ADMIN_PERMISSIONS,
        }
    }

    /// True when the role carries every bit of `flag` (`flag == 0` never
    /// grants: the non-staff zero mask must not pass any action).
    pub fn has_permission(self, flag: u64) -> bool {
        flag != 0 && self.permissions() & flag == flag
    }

    /// True when the role may perform the fine-grained `action` (D35).
    pub fn allows(self, action: AdminAction) -> bool {
        self.has_permission(action.required_permission())
    }

    /// Parse a CLI role name (`admin`, `moderator`, `none`).
    pub fn parse_cli(s: &str) -> Option<UserRole> {
        match s.to_ascii_lowercase().as_str() {
            "admin" => Some(UserRole::Admin),
            "moderator" => Some(UserRole::Moderator),
            "none" => Some(UserRole::None),
            _ => None,
        }
    }
}
