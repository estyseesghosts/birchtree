use super::totp;

/// How many backup codes one confirmation mints (upstream mints 16
/// recovery codes; 10 keeps the ceremony short while staying brute-force
/// safe at 64 bits each — see [`BACKUP_CODE_BYTES`]).
pub const BACKUP_CODE_COUNT: usize = 10;
/// Random bytes per backup code (64 bits; stored only as SHA-256).
pub const BACKUP_CODE_BYTES: usize = 8;

/// SHA-256 hex digest of a normalized backup code (the at-rest form).
pub fn hash_backup_code(code: &str) -> String {
    let normalized = code.trim().to_ascii_lowercase();
    hex::encode(ring::digest::digest(
        &ring::digest::SHA256,
        normalized.as_bytes(),
    ))
}

/// Consume a backup code: on a constant-time hash match the entry is removed
/// (single-use) and `true` is returned; otherwise the list is untouched.
/// The caller persists the mutated record on success.
pub fn consume_backup_code_hash(hashes: &mut Vec<String>, code: &str) -> bool {
    let candidate = hash_backup_code(code);
    let mut index: Option<usize> = None;
    for (i, stored) in hashes.iter().enumerate() {
        let hit = stored.len() == candidate.len()
            && totp::constant_time_equal(stored.as_bytes(), candidate.as_bytes());
        if hit && index.is_none() {
            index = Some(i);
        }
    }
    match index {
        Some(i) => {
            hashes.remove(i);
            true
        }
        None => false,
    }
}
