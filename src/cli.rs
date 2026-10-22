use birchtree::config::Config;
use std::net::IpAddr;

/// Parsed command-line arguments.
#[derive(Default)]
pub(crate) struct Args {
    pub(crate) positional: Vec<String>,
    pub(crate) username: Option<String>,
    pub(crate) password: Option<String>,
    pub(crate) email: Option<String>,
    pub(crate) display_name: Option<String>,
    pub(crate) role: Option<String>,
    pub(crate) harness_mode: Option<String>,
    pub(crate) text: Option<String>,
    pub(crate) hint: Option<String>,
    pub(crate) id: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) file: Option<String>,
    pub(crate) locale: Option<String>,
    pub(crate) shortcode: Option<String>,
    pub(crate) category: Option<String>,
    pub(crate) hidden: bool,
}

/// Parse the `--max-body-size=` value.
///
/// Pure helper extracted so unit tests can cover the invalid-input path
/// without hitting the `eprintln!` + `process::exit(2)` in [`parse_args`].
/// The error string is byte-identical to the message `parse_args` printed
/// inline before the extraction.
pub(crate) fn parse_max_body_size(value: &str) -> Result<usize, String> {
    value
        .parse::<usize>()
        .map_err(|_| format!("invalid --max-body-size: {value}"))
}

/// Parse the `--max-status-chars=` value.
///
/// Pure helper extracted so unit tests can cover the invalid-input path
/// without hitting the `eprintln!` + `process::exit(2)` in [`parse_args`].
/// Keeps the original acceptance exactly (`parse` + `>= 1` filter, no upper
/// bound here) and the original error string byte-identical.
pub(crate) fn parse_max_status_chars(value: &str) -> Result<usize, String> {
    value
        .parse::<usize>()
        .ok()
        .filter(|count| *count >= 1)
        .ok_or_else(|| format!("invalid --max-status-chars (want an integer 1..=3000): {value}"))
}

/// Parse one `--trusted-proxy=` value.
///
/// Pure helper extracted so unit tests can cover the invalid-input path
/// without hitting the `eprintln!` + `process::exit(2)` in [`parse_args`].
/// The error string is byte-identical to the message `parse_args` printed
/// inline before the extraction.
pub(crate) fn parse_trusted_proxy(value: &str) -> Result<IpAddr, String> {
    value
        .parse::<IpAddr>()
        .map_err(|_| format!("invalid --trusted-proxy address: {value}"))
}

pub(crate) fn parse_args(cfg: &mut Config) -> Args {
    let mut args = Args::default();
    for arg in std::env::args().skip(1) {
        if let Some(_path) = arg.strip_prefix("--config=") {
            // Consumed before parsing (sets the base config this call
            // overrides); see main().
        } else if let Some(v) = arg.strip_prefix("--public-url=") {
            cfg.public_url = v.to_string();
        } else if let Some(v) = arg.strip_prefix("--data-dir=") {
            cfg.data_dir = v.into();
        } else if let Some(v) = arg.strip_prefix("--listen=") {
            cfg.listen = v.parse().unwrap_or_else(|_| {
                eprintln!("invalid --listen address: {v}");
                std::process::exit(2);
            });
        } else if let Some(v) = arg.strip_prefix("--trusted-proxy=") {
            cfg.trusted_proxies
                .push(parse_trusted_proxy(v).unwrap_or_else(|err| {
                    eprintln!("{err}");
                    std::process::exit(2);
                }));
        } else if let Some(v) = arg.strip_prefix("--max-body-size=") {
            cfg.max_body_size = parse_max_body_size(v).unwrap_or_else(|err| {
                eprintln!("{err}");
                std::process::exit(2);
            });
        } else if let Some(v) = arg.strip_prefix("--max-status-chars=") {
            cfg.max_status_chars = parse_max_status_chars(v).unwrap_or_else(|err| {
                eprintln!("{err}");
                std::process::exit(2);
            });
        } else if let Some(v) = arg.strip_prefix("--username=") {
            args.username = Some(v.to_string());
        } else if let Some(v) = arg.strip_prefix("--password=") {
            args.password = Some(v.to_string());
        } else if let Some(v) = arg.strip_prefix("--email=") {
            args.email = Some(v.to_string());
        } else if let Some(v) = arg.strip_prefix("--display-name=") {
            args.display_name = Some(v.to_string());
        } else if let Some(v) = arg.strip_prefix("--role=") {
            args.role = Some(v.to_string());
        } else if let Some(v) = arg.strip_prefix("--text=") {
            args.text = Some(v.to_string());
        } else if let Some(v) = arg.strip_prefix("--hint=") {
            args.hint = Some(v.to_string());
        } else if let Some(v) = arg.strip_prefix("--id=") {
            args.id = Some(v.to_string());
        } else if let Some(v) = arg.strip_prefix("--name=") {
            args.name = Some(v.to_string());
        } else if let Some(v) = arg.strip_prefix("--file=") {
            args.file = Some(v.to_string());
        } else if let Some(v) = arg.strip_prefix("--shortcode=") {
            args.shortcode = Some(v.to_string());
        } else if let Some(v) = arg.strip_prefix("--category=") {
            args.category = Some(v.to_string());
        } else if arg == "--hidden" {
            args.hidden = true;
        } else if let Some(v) = arg.strip_prefix("--locale=") {
            args.locale = Some(v.to_string());
        } else if let Some(v) = arg.strip_prefix("--site-title=") {
            cfg.site_title = v.to_string();
        } else if let Some(v) = arg.strip_prefix("--site-description=") {
            cfg.site_description = v.to_string();
        } else if let Some(v) = arg.strip_prefix("--site-contact-email=") {
            cfg.site_contact_email = v.to_string();
        } else if let Some(v) = arg.strip_prefix("--streaming-url=") {
            cfg.streaming_api_base_url = Some(v.trim_end_matches('/').to_string());
        } else if let Some(v) = arg.strip_prefix("--site-contact-username=") {
            cfg.site_contact_username = Some(v.trim_start_matches('@').to_string());
        } else if arg == "--federation-allow-http" {
            cfg.federation_allow_http = true;
        } else if arg == "--federation-authorized-fetch" {
            cfg.federation_authorized_fetch = true;
        } else if let Some(v) = arg.strip_prefix("--federation-extra-ca=") {
            cfg.federation_extra_ca = Some(std::path::PathBuf::from(v));
        } else if let Some(v) = arg.strip_prefix("--federation-allow-private=") {
            cfg.federation_allowlist = birchtree::federation::policy::parse_allowlist(v)
                .unwrap_or_else(|err| {
                    eprintln!("invalid --federation-allow-private: {err}");
                    std::process::exit(2);
                });
        } else if arg == "--debug-weak-rate-limits" {
            cfg.rate_limit_multiplier = 10;
        } else if let Some(v) = arg.strip_prefix("--rate-limit-multiplier=") {
            cfg.rate_limit_multiplier = v.parse().ok().filter(|m| *m >= 1).unwrap_or_else(|| {
                eprintln!("invalid --rate-limit-multiplier (want an integer >= 1): {v}");
                std::process::exit(2);
            });
        } else if arg == "--lab" {
            cfg.mode = birchtree::config::RuntimeMode::Lab;
        } else if let Some(v) = arg.strip_prefix("--unsafe-production-override=") {
            cfg.unsafe_production_override = Some(v.to_string());
        } else if let Some(v) = arg.strip_prefix("--secret-keyring-file=") {
            cfg.secret_keyring_file = Some(std::path::PathBuf::from(v));
        } else if let Some(v) = arg.strip_prefix("--secret-keyring-env=") {
            cfg.secret_keyring_env = Some(v.to_string());
        } else if arg == "--registration-open" {
            cfg.registration_open = true;
        } else if arg == "--registration-closed" {
            cfg.registration_open = false;
        } else if arg == "--registration-require-approval" {
            cfg.registration_requires_approval = true;
        } else if let Some(v) = arg.strip_prefix("--search-mode=") {
            cfg.search_mode = birchtree::search::SearchMode::parse(v).unwrap_or_else(|| {
                eprintln!("invalid --search-mode (want embedded|database|off): {v}");
                std::process::exit(2);
            });
        } else if arg == "--no-transcode" {
            cfg.transcoding_enabled = false;
        } else if let Some(v) = arg.strip_prefix("--ffmpeg-path=") {
            cfg.ffmpeg_path = v.to_string();
        } else if let Some(v) = arg.strip_prefix("--webp-quality=") {
            cfg.webp_quality = v.parse().unwrap_or_else(|_| {
                eprintln!("invalid --webp-quality (want 0-100): {v}");
                std::process::exit(2);
            });
        } else if let Some(v) = arg.strip_prefix("--vp9-crf=") {
            cfg.vp9_crf = v.parse().unwrap_or_else(|_| {
                eprintln!("invalid --vp9-crf (want 0-63): {v}");
                std::process::exit(2);
            });
        } else if let Some(v) = arg.strip_prefix("--vp9-cpu-used=") {
            cfg.vp9_cpu_used = v.parse().unwrap_or_else(|_| {
                eprintln!("invalid --vp9-cpu-used (want 0-5): {v}");
                std::process::exit(2);
            });
        } else if let Some(v) = arg.strip_prefix("--opus-bitrate-kbps=") {
            cfg.opus_bitrate_kbps = v.parse().unwrap_or_else(|_| {
                eprintln!("invalid --opus-bitrate-kbps (want an integer > 0): {v}");
                std::process::exit(2);
            });
        } else if let Some(v) = arg.strip_prefix("--transcode-wall-time-secs=") {
            cfg.transcode_wall_time_secs = v.parse().unwrap_or_else(|_| {
                eprintln!("invalid --transcode-wall-time-secs: {v}");
                std::process::exit(2);
            });
        } else if let Some(v) = arg.strip_prefix("--transcode-max-output-bytes=") {
            cfg.transcode_max_output_bytes = v.parse().unwrap_or_else(|_| {
                eprintln!("invalid --transcode-max-output-bytes: {v}");
                std::process::exit(2);
            });
        } else if let Some(v) = arg.strip_prefix("--transcode-max-memory-bytes=") {
            cfg.transcode_max_memory_bytes = v.parse().unwrap_or_else(|_| {
                eprintln!("invalid --transcode-max-memory-bytes: {v}");
                std::process::exit(2);
            });
        } else if let Some(v) = arg.strip_prefix("--vapid-public-key=") {
            cfg.vapid_public_key = v.to_string();
        } else if let Some(v) = arg.strip_prefix("--vapid-private-key=") {
            cfg.vapid_private_key = v.to_string();
        } else if let Some(v) = arg.strip_prefix("--smtp-url=") {
            cfg.smtp_url = Some(v.to_string());
        } else if let Some(v) = arg.strip_prefix("--mail-from=") {
            cfg.mail_from = Some(v.to_string());
        } else if let Some(v) = arg.strip_prefix("--relay-inbox=") {
            cfg.relay_inboxes.push(v.to_string());
        } else if let Some(v) = arg.strip_prefix("--password-reset-ttl-hours=") {
            cfg.password_reset_ttl_hours = v.parse().unwrap_or_else(|_| {
                eprintln!("invalid --password-reset-ttl-hours (want an integer 1..=72): {v}");
                std::process::exit(2);
            });
        } else if let Some(v) = arg.strip_prefix("--site-terms-file=") {
            cfg.site_terms_file = Some(std::path::PathBuf::from(v));
        } else if let Some(v) = arg.strip_prefix("--site-privacy-file=") {
            cfg.site_privacy_file = Some(std::path::PathBuf::from(v));
        } else if let Some(v) = arg.strip_prefix("--site-extended-file=") {
            cfg.site_extended_file = Some(std::path::PathBuf::from(v));
        } else if let Some(v) = arg.strip_prefix("--web-client-dir=") {
            cfg.web_client_dir = Some(std::path::PathBuf::from(v));
        } else if let Some(v) = arg.strip_prefix("--license-policy=") {
            cfg.license_policy = v.to_string();
        } else if let Some(v) = arg.strip_prefix("--place-policy=") {
            cfg.place_policy = v.to_string();
        } else if let Some(v) = arg.strip_prefix("--alt-text-policy=") {
            cfg.alt_text_policy = v.to_string();
        } else if let Some(v) = arg.strip_prefix("--mode=") {
            args.harness_mode = Some(v.to_string());
        } else if arg.starts_with("--") {
            eprintln!("unknown flag: {arg}");
            std::process::exit(2);
        } else {
            args.positional.push(arg);
        }
    }
    args
}

#[cfg(test)]
mod tests {
    use super::{parse_max_body_size, parse_max_status_chars, parse_trusted_proxy};

    #[test]
    fn rejects_non_numeric_max_body_size() {
        let err = parse_max_body_size("abc").unwrap_err();
        assert_eq!(err, "invalid --max-body-size: abc");
    }

    #[test]
    fn rejects_negative_max_body_size() {
        let err = parse_max_body_size("-1").unwrap_err();
        assert_eq!(err, "invalid --max-body-size: -1");
    }

    #[test]
    fn accepts_valid_max_body_size() {
        assert_eq!(parse_max_body_size("1048576"), Ok(1_048_576));
    }

    #[test]
    fn rejects_zero_max_status_chars() {
        let err = parse_max_status_chars("0").unwrap_err();
        assert_eq!(
            err,
            "invalid --max-status-chars (want an integer 1..=3000): 0"
        );
    }

    #[test]
    fn rejects_non_numeric_max_status_chars() {
        let err = parse_max_status_chars("many").unwrap_err();
        assert_eq!(
            err,
            "invalid --max-status-chars (want an integer 1..=3000): many"
        );
    }

    #[test]
    fn accepts_valid_max_status_chars() {
        assert_eq!(parse_max_status_chars("500"), Ok(500));
    }

    #[test]
    fn rejects_hostname_trusted_proxy() {
        let err = parse_trusted_proxy("example.com").unwrap_err();
        assert_eq!(err, "invalid --trusted-proxy address: example.com");
    }

    #[test]
    fn rejects_empty_trusted_proxy() {
        let err = parse_trusted_proxy("").unwrap_err();
        assert_eq!(err, "invalid --trusted-proxy address: ");
    }

    #[test]
    fn accepts_ipv4_and_ipv6_trusted_proxy() {
        assert!(parse_trusted_proxy("127.0.0.1").is_ok());
        assert!(parse_trusted_proxy("::1").is_ok());
    }
}
