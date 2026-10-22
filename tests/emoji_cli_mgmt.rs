//! CLI custom-emoji authoring tests: drive the real server binary's
//! `add-emoji` / `remove-emoji` (`rm-emoji`) / `list-emojis` subcommands
//! against a fresh temp data-dir (never the real store).

use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_birchtree")
}

/// A real 1x1 PNG (same encoder stack the server validates with — the
/// parser's `IHDR` checks stay strict, so synthetic magic bytes are out).
fn png_bytes() -> Vec<u8> {
    let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        1,
        1,
        image::Rgba([10, 20, 30, 255]),
    ));
    let mut cursor = std::io::Cursor::new(Vec::new());
    img.write_to(&mut cursor, image::ImageFormat::Png)
        .expect("png");
    cursor.into_inner()
}

fn run(dir: &tempfile::TempDir, args: &[&str]) -> Output {
    let mut cmd = Command::new(bin());
    cmd.arg(format!("--data-dir={}", dir.path().display()))
        .arg("--public-url=https://example.social");
    for arg in args {
        cmd.arg(arg);
    }
    cmd.output().expect("spawn CLI subcommand")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn write_image(dir: &tempfile::TempDir, name: &str, bytes: &[u8]) -> String {
    let path = dir.path().join(name);
    std::fs::write(&path, bytes).expect("write fixture image");
    path.display().to_string()
}

#[test]
fn add_then_list_shows_it() {
    let dir = tempfile::tempdir().unwrap();
    let image = write_image(&dir, "parrot.png", &png_bytes());

    let out = run(
        &dir,
        &[
            "add-emoji",
            "--shortcode=party_parrot",
            &format!("--file={image}"),
            "--category=fun",
        ],
    );
    assert!(
        out.status.success(),
        "add-emoji should exit 0: {}",
        stderr(&out)
    );
    assert!(stdout(&out).contains("party_parrot"));

    let out = run(&dir, &["list-emojis"]);
    assert!(
        out.status.success(),
        "list-emojis should exit 0: {}",
        stderr(&out)
    );
    let listed = stdout(&out);
    assert!(
        listed.contains("party_parrot") && listed.contains("category=fun"),
        "list should show the added emoji with its category, got {listed:?}"
    );
}

#[test]
fn readd_replaces_category() {
    let dir = tempfile::tempdir().unwrap();
    let image = write_image(&dir, "wave.png", &png_bytes());

    for category in ["greetings", "hellos"] {
        let out = run(
            &dir,
            &[
                "add-emoji",
                "--shortcode=wave_hi",
                &format!("--file={image}"),
                &format!("--category={category}"),
            ],
        );
        assert!(
            out.status.success(),
            "re-add should exit 0: {}",
            stderr(&out)
        );
    }

    let listed = stdout(&run(&dir, &["list-emojis"]));
    assert!(
        listed.contains("category=hellos"),
        "second add should replace the first, got {listed:?}"
    );
    assert!(
        !listed.contains("category=greetings"),
        "stale category must be gone, got {listed:?}"
    );
}

#[test]
fn remove_then_rm_alias_missing_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let image = write_image(&dir, "bye.png", &png_bytes());

    let out = run(
        &dir,
        &[
            "add-emoji",
            "--shortcode=bye_wave",
            &format!("--file={image}"),
        ],
    );
    assert!(out.status.success(), "setup add: {}", stderr(&out));

    // `rm-emoji` alias removes the existing row.
    let out = run(&dir, &["rm-emoji", "--shortcode=bye_wave"]);
    assert!(
        out.status.success(),
        "rm-emoji alias should exit 0: {}",
        stderr(&out)
    );
    assert!(!stdout(&run(&dir, &["list-emojis"])).contains("bye_wave"));

    // Removing a missing shortcode is not-found (exit 1), not usage (exit 2).
    let out = run(&dir, &["remove-emoji", "--shortcode=bye_wave"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "remove-missing should exit 1, stderr: {}",
        stderr(&out)
    );
    assert!(stderr(&out).contains("bye_wave"));
}

#[test]
fn add_bad_shortcode_fails() {
    let dir = tempfile::tempdir().unwrap();
    let image = write_image(&dir, "ok.png", &png_bytes());

    // Too short (minimum is 2 chars).
    let out = run(
        &dir,
        &["add-emoji", "--shortcode=x", &format!("--file={image}")],
    );
    assert!(
        !out.status.success(),
        "bad shortcode must fail, stdout: {}",
        stdout(&out)
    );
    assert!(!stderr(&out).is_empty(), "failure must explain on stderr");

    // Illegal characters.
    let out = run(
        &dir,
        &[
            "add-emoji",
            "--shortcode=bad-code!",
            &format!("--file={image}"),
        ],
    );
    assert!(!out.status.success(), "illegal shortcode must fail");

    // Invalid image bytes.
    let text = write_image(&dir, "note.txt", b"not an image at all");
    let out = run(
        &dir,
        &[
            "add-emoji",
            "--shortcode=not_an_image",
            &format!("--file={text}"),
        ],
    );
    assert!(!out.status.success(), "non-image bytes must fail");
    assert!(!stderr(&out).is_empty());

    // Missing file.
    let out = run(
        &dir,
        &[
            "add-emoji",
            "--shortcode=ghost_file",
            "--file=/nonexistent/emoji.png",
        ],
    );
    assert!(!out.status.success(), "missing file must fail");
}
