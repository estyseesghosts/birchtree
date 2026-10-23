use axum::body::Body;
use axum::extract::multipart::Field;
use axum::extract::{FromRequest, Multipart, Request};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use super::super::media::PacedBody;
use super::support::{AVATAR_LIMIT, HEADER_LIMIT, PROFILE_IMAGE_MIME_TYPES, validation_error};
use crate::api::mastodon::ApiState;
use crate::api::mastodon::errors::{ApiError, ApiResult};
use crate::application::media::human_size;
use crate::application::ports::ProfileImageData;
use crate::media::staging::StagingGuard;
use crate::media::upload_gate;
use crate::media::upload_spool::{SpoolError, UploadSpool};

/// Multipart fields of an `update_credentials` body: every text part flattens
/// into the same pairs the urlencoded path parses (order preserved, so the
/// `fields_attributes[][name]` sequencing keeps working); `avatar`/`header`
/// file parts stream into capped spool files (`Some` spool when the part is
/// present — empty spool when it is present but empty, the clearing
/// signal). Dropping the fields removes the spool files.
pub(super) struct CredentialMultipart {
    pub(super) pairs: Vec<(String, String)>,
    pub(super) avatar: Option<UploadSpool>,
    pub(super) header: Option<UploadSpool>,
}

/// Parse a `multipart/form-data` credentials body. A malformed frame is a
/// `400` (like the media upload path); content problems inside a well-formed
/// part are `422`s from the image pipeline below, never here.
///
/// File parts stream into a spool capped at the same ceiling the image
/// pipeline enforces (`max_file_size` and the per-image limit, whichever is
/// smaller — the media-ingest ceiling), so an oversized part is rejected
/// before it is resident. The body is paced one frame per task wake (the
/// media upload path's `PacedBody`), so a fast sender cannot make the whole
/// part resident before the spool cap trips.
pub(super) async fn credential_multipart(
    state: &ApiState,
    request: Request,
) -> ApiResult<CredentialMultipart> {
    let (parts, body) = request.into_parts();
    let paced = Body::from_stream(PacedBody::new(body.into_data_stream()));
    let request = Request::from_parts(parts, paced);
    let mut multipart = Multipart::from_request(request, state)
        .await
        .map_err(|err| ApiError::bad_request(format!("Invalid multipart body: {err}")))?;
    let mut fields = CredentialMultipart {
        pairs: Vec::new(),
        avatar: None,
        header: None,
    };
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|err| ApiError::bad_request(format!("Invalid multipart body: {err}")))?
    {
        let name = field.name().map(str::to_string);
        match name.as_deref() {
            Some("avatar") => {
                fields.avatar = Some(
                    spool_field(
                        state,
                        field,
                        "Avatar",
                        state.server.max_file_size.min(AVATAR_LIMIT),
                    )
                    .await?,
                );
            }
            Some("header") => {
                fields.header = Some(
                    spool_field(
                        state,
                        field,
                        "Header",
                        state.server.max_file_size.min(HEADER_LIMIT),
                    )
                    .await?,
                );
            }
            Some(name) => {
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|err| ApiError::bad_request(format!("Invalid multipart body: {err}")))?
                    .to_vec();
                fields.pairs.push((
                    name.to_string(),
                    String::from_utf8_lossy(&bytes).into_owned(),
                ));
            }
            None => {}
        }
    }
    Ok(fields)
}

/// Stream one avatar/header part into a capped spool file. The cap is
/// enforced as the bytes arrive, so an oversized part is rejected before it
/// is resident, and every failure path drops the spool (which deletes its
/// file).
async fn spool_field(
    state: &ApiState,
    mut field: Field<'_>,
    label: &str,
    limit: u64,
) -> ApiResult<UploadSpool> {
    let root = state.context.blobs.root_path();
    let mut spool = UploadSpool::create(root.as_deref(), limit)
        .await
        .map_err(|err| spool_error(label, err))?;
    while let Some(chunk) = field
        .chunk()
        .await
        .map_err(|err| ApiError::bad_request(format!("Invalid multipart body: {err}")))?
    {
        spool
            .write_chunk(&chunk)
            .await
            .map_err(|err| spool_error(label, err))?;
    }
    spool
        .finish()
        .await
        .map_err(|err| spool_error(label, err))?;
    Ok(spool)
}

/// Map a spool failure onto the file's existing 422 shape (never 415/500):
/// an over-cap part reports the same size message the image pipeline would.
fn spool_error(label: &str, err: SpoolError) -> ApiError {
    match err {
        SpoolError::TooLarge { limit } => ApiError::unprocessable(format!(
            "Validation failed: {label} file size must be less than {}",
            human_size(limit)
        )),
        SpoolError::Storage(err) => ApiError::from(err),
    }
}

pub(super) fn is_multipart(request: &Request) -> bool {
    request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("multipart/form-data"))
}

/// Resolve one profile-image slot to its `AccountProfileUpdate` presence:
/// `None` = absent (leave stored), `Some(None)` = present-but-empty (clear to
/// the default), `Some(Some(..))` = a staged upload (validate + stage now).
///
/// A present-but-non-empty urlencoded/JSON value carries no bytes and can
/// never become an image, so it 422s instead of silently ignoring the intent
/// (upstream Paperclip likewise refuses a string where a file belongs).
///
/// The spool is read back bounded (never more than the cap it was created
/// with); sniff/decode runs on the bounded media work gate, never on the
/// async worker; bytes stage through `staging`, so dropping it (a later
/// field validation or a failed account update) deletes the new keys.
pub(super) async fn profile_image_slot(
    state: &ApiState,
    label: &str,
    limit: u64,
    kind: &str,
    upload: Option<UploadSpool>,
    field: Option<&str>,
    staging: &mut StagingGuard<'_>,
) -> Result<Option<Option<ProfileImageData>>, Response> {
    match upload {
        Some(spool) if !spool.is_empty() => {
            let bytes = spool
                .read_all()
                .map_err(|_| ApiError::internal().into_response())?;
            let max_file_size = state.server.max_file_size;
            let owned_label = label.to_string();
            let (bytes, mime_type, extension) = upload_gate::media_work_gate()
                .run(move || {
                    validate_profile_image(bytes.len(), &bytes, &owned_label, limit, max_file_size)
                        .map(|(mime_type, extension)| (bytes, mime_type, extension))
                })
                .await
                .map_err(|err| ApiError::from(err).into_response())??;
            let hex = crate::application::crypto::random_token_hex(8);
            let key = format!("{}/{}/{}/{}.{extension}", &hex[0..2], &hex[2..4], hex, kind);
            staging
                .put(&key, &bytes)
                .map_err(|err| ApiError::from(err).into_response())?;
            Ok(Some(Some(ProfileImageData {
                key,
                content_type: mime_type.to_string(),
            })))
        }
        Some(_) => Ok(Some(None)),
        None => match field {
            None => Ok(None),
            Some(raw) if raw.is_empty() => Ok(Some(None)),
            Some(_) => Err(validation_error(
                format!("Validation failed: {label} content type is invalid"),
                json!({}),
            )),
        },
    }
}

/// Sniff, cap and decode-check one avatar/header upload: the declared content
/// type is never trusted (magic bytes decide, exactly like media uploads).
/// Failures are the file's existing 422 shape, never 415/500.
fn validate_profile_image(
    len: usize,
    bytes: &[u8],
    label: &str,
    limit: u64,
    max_file_size: u64,
) -> Result<(&'static str, &'static str), Response> {
    let sniffed = crate::media::ingest::sniff(bytes).filter(|sniffed| {
        PROFILE_IMAGE_MIME_TYPES.contains(&sniffed.mime_type) && sniffed.r#type == "image"
    });
    let Some(sniffed) = sniffed else {
        return Err(validation_error(
            format!("Validation failed: {label} content type is invalid"),
            json!({}),
        ));
    };
    // Both caps apply: the operator's `max_file_size` and the upstream
    // per-image limit, whichever is smaller.
    let cap = max_file_size.min(limit);
    if len as u64 > cap {
        let human = human_size(cap);
        return Err(validation_error(
            format!("Validation failed: {label} file size must be less than {human}"),
            json!({}),
        ));
    }
    // Decode-check through the media path (undecodable bytes are an invalid
    // upload, not a server fault). GIF/BMP previews are PNG; only the
    // verdict matters here — avatars serve the staged original.
    match crate::media::processing::process(sniffed.mime_type, bytes) {
        Ok(_) => Ok((sniffed.mime_type, sniffed.extension)),
        Err(crate::media::processing::ProcessError::Unidentifiable(_)) => Err(validation_error(
            format!("Validation failed: {label} is invalid"),
            json!({}),
        )),
        Err(_) => Err(ApiError::internal().into_response()),
    }
}

/// Parse `fields_attributes` profile fields from flattened params.
///
/// Accepts both the form encoding (`fields_attributes[][name|value]`) and the
/// JSON-flattened indexed encoding (`fields_attributes[N][name|value]`,
/// like the filters' `keywords_attributes[N][...]`). Upstream rules
/// (`account.rb:133` max 4 fields; `Account::Field#sanitize` strip + 255
/// chars; `EmptyProfileFieldNamesValidator`) are enforced here with 422s;
/// persistence is the named follow-up (no `Account` storage yet), so valid
/// fields are returned only for validation.
pub(super) fn parse_profile_fields(
    pairs: &[(String, String)],
) -> Result<Vec<(String, String)>, Response> {
    use std::collections::BTreeMap;
    let mut indexed: BTreeMap<String, (Option<String>, Option<String>)> = BTreeMap::new();
    let mut ordered: Vec<(Option<String>, Option<String>)> = Vec::new();
    for (key, value) in pairs {
        if let Some(rest) = key.strip_prefix("fields_attributes[]") {
            // Form encoding: `fields_attributes[][name]` — pairs arrive in
            // order; a `[name]` starts a new field, `[value]` completes it.
            if rest == "[name]" {
                ordered.push((Some(value.clone()), None));
            } else if rest == "[value]" {
                match ordered.last_mut() {
                    Some(last) if last.1.is_none() => last.1 = Some(value.clone()),
                    _ => ordered.push((None, Some(value.clone()))),
                }
            }
            continue;
        }
        if let Some(rest) = key.strip_prefix("fields_attributes[") {
            // Indexed encoding: `fields_attributes[N][name|value]`.
            let Some((index, field)) = rest.split_once("][") else {
                continue;
            };
            let field = field.trim_end_matches(']');
            if field != "name" && field != "value" {
                continue;
            }
            let entry = indexed.entry(index.to_string()).or_insert((None, None));
            if field == "name" {
                entry.0 = Some(value.clone());
            } else {
                entry.1 = Some(value.clone());
            }
        }
    }
    for (_, entry) in indexed {
        ordered.push(entry);
    }
    // Rails drops fully-blank rows (`reject_if: :all_blank` on the nested
    // attributes). Over-long names/values are truncated, not rejected
    // (`Account::Field#sanitize`: strip + first 255 local chars).
    let truncate = |raw: String| {
        let stripped = raw.trim();
        stripped.chars().take(255).collect::<String>()
    };
    let fields: Vec<(String, String)> = ordered
        .into_iter()
        .map(|(name, value)| {
            (
                truncate(name.unwrap_or_default()),
                truncate(value.unwrap_or_default()),
            )
        })
        .filter(|(name, value)| !(name.is_empty() && value.is_empty()))
        .collect();
    // `account.rb:133`: at most 4 fields (Rails length-validator message).
    if fields.len() > 4 {
        return Err(validation_error(
            "Validation failed: Fields is too long (maximum is 4 characters)",
            json!({}),
        ));
    }
    // `EmptyProfileFieldNamesValidator`: a value without a label 422s.
    if fields
        .iter()
        .any(|(name, value)| name.is_empty() && !value.is_empty())
    {
        return Err(validation_error(
            "Validation failed: Fields contains values with missing labels",
            json!({}),
        ));
    }
    Ok(fields)
}
