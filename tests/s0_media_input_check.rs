//! S0 check: the canonical [`UploadFile`][media_input] in
//! `application::media_input` keeps the `Bytes` `len`/`prefix` behavior the
//! upload path relies on for content sniffing.
//!
//! NOTE: needs `pub mod media_input;` in `src/application/mod.rs` (owned by
//! the parent; S0 wiring) before this target builds.
//!
//! [media_input]: birchtree::application::media_input

use birchtree::application::media_input::UploadFile;
use birchtree::application::media_input::UploadInput;
use birchtree::domain::Id;
use birchtree::media::upload_spool::SNIFF_PREFIX_BYTES;

#[test]
fn bytes_len_reports_resident_length() {
    let file = UploadFile::Bytes(b"hello");
    assert_eq!(file.len(), 5);
    let empty = UploadFile::Bytes(b"");
    assert_eq!(empty.len(), 0);
}

#[test]
fn bytes_prefix_returns_leading_slice() {
    let bytes: &[u8] = b"\x89PNG\r\n\x1a\nrest-of-file";
    let file = UploadFile::Bytes(bytes);
    assert_eq!(file.prefix(), bytes);
    assert_eq!(UploadFile::Bytes(b"").prefix(), b"".as_slice());
}

#[test]
fn bytes_prefix_is_capped_at_sniff_limit() {
    let big = vec![0x41u8; SNIFF_PREFIX_BYTES + 16];
    let file = UploadFile::Bytes(&big);
    assert_eq!(file.len(), (SNIFF_PREFIX_BYTES + 16) as u64);
    assert_eq!(file.prefix().len(), SNIFF_PREFIX_BYTES);
    assert_eq!(file.prefix(), &big[..SNIFF_PREFIX_BYTES]);
}

#[test]
fn upload_input_borrows_without_copying() {
    let bytes: &[u8] = b"fake-image-bytes";
    let input = UploadInput {
        now_ms: 1_700_000_000_000,
        account_id: Id::new(42),
        file: UploadFile::Bytes(bytes),
        thumbnail_bytes: None,
        description: "alt text",
        focus: Some("0.1,-0.2"),
        delay: false,
        max_file_size: 16_777_216,
        config: None,
        license_policy: "allow",
        license: None,
        alt_text_policy: "allow",
    };
    assert_eq!(input.file.len(), bytes.len() as u64);
    assert_eq!(input.description, "alt text");
    assert_eq!(input.account_id, Id::new(42));
}
