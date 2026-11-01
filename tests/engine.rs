use rars::ReadCancellation;
use rawr::engine::{self, safe_relative, AppError, ExtractRequest, OpenedArchive, ProgressFn};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rawr-test-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn quiet() -> ProgressFn {
    Arc::new(|_| {})
}

fn open(path: &Path, password: Option<&str>) -> Result<OpenedArchive, AppError> {
    engine::open(path, password, &ReadCancellation::new())
}

fn extract_all(path: &Path, out: &Path, password: Option<&str>) -> engine::ExtractReport {
    engine::extract(
        ExtractRequest {
            archive: path,
            out_dir: out,
            password,
            selected: None,
            overwrite: false,
        },
        &ReadCancellation::new(),
        quiet(),
    )
    .unwrap()
}

fn assert_sizes_match(listing: &OpenedArchive, out: &Path) {
    for entry in listing.entries.iter().filter(|e| !e.is_directory && !e.is_link) {
        let written = fs::metadata(out.join(&entry.name))
            .unwrap_or_else(|e| panic!("{}: {e}", entry.name))
            .len();
        assert_eq!(written, entry.unpacked_size, "{}", entry.name);
    }
}

#[test]
fn old_names_read_in_the_chosen_encoding() {
    assert_eq!(engine::legacy_text(&[0x80, b'.', b't'], false, 0), "Ç.t");
    assert_eq!(engine::legacy_text(&[0x80, b'.', b't'], false, 3), "А.t");
    assert_eq!(engine::legacy_text("dir\\ok".as_bytes(), false, 3), "dir/ok");
}

#[test]
fn rejects_names_unsafe_on_windows() {
    for name in [
        "../evil.txt",
        "safe/../../evil.txt",
        "/abs.txt",
        "C:/evil.txt",
        "C:evil.txt",
        "a.txt:stream",
        "CON",
        "dir/nul.txt",
        "COM1.log",
        "trailing.",
        "trailing ",
        "bad?name",
        "",
    ] {
        assert!(safe_relative(name).is_err(), "{name:?}");
    }
    for name in ["a/b.txt", "CONSOLE.txt", "com.txt", "résumé 日本語.txt"] {
        assert!(safe_relative(name).is_ok(), "{name:?}");
    }
}

#[test]
fn extracts_each_generation() {
    for name in ["README.RAR", "SOLID.RAR", "stored_rar50.rar", "stored_rar70.rar"] {
        let path = fixtures().join(name);
        let listing = open(&path, None).unwrap();
        assert!(!listing.entries.is_empty(), "{name}");
        let out = scratch(&format!("gen-{name}"));
        let report = extract_all(&path, &out, None);
        assert!(report.written > 0, "{name}");
        assert_sizes_match(&listing, &out);
        assert!(
            fs::read_dir(&out)
                .unwrap()
                .flatten()
                .all(|e| !e.file_name().to_string_lossy().starts_with(".rawr-tmp")),
            "{name} left staging behind"
        );
    }
}

#[test]
fn multi_part_found_from_any_part() {
    for name in ["stored_volume.part1.rar", "stored_volume.part3.rar"] {
        let listing = open(&fixtures().join(name), None).unwrap();
        assert_eq!(listing.parts, 4, "{name}");
    }
    let path = fixtures().join("stored_volume.part1.rar");
    let listing = open(&path, None).unwrap();
    let out = scratch("multi");
    extract_all(&path, &out, None);
    assert_sizes_match(&listing, &out);
}

#[test]
fn encrypted_needs_the_right_password() {
    let path = fixtures().join("encrypted_multivol.part1.rar");
    let listing = open(&path, None).unwrap();
    assert!(listing.any_encrypted);
    let cancel = ReadCancellation::new();
    let wrong = engine::test(&path, Some("nope"), &cancel, quiet()).unwrap_err();
    assert_eq!(wrong.kind, "bad_password");
    let missing = engine::test(&path, None, &cancel, quiet()).unwrap_err();
    assert_eq!(missing.kind, "password_required");
    assert!(engine::test(&path, Some("password"), &cancel, quiet()).unwrap() > 0);
}

#[test]
fn selection_and_skip_existing() {
    let path = fixtures().join("stored_rar50.rar");
    let listing = open(&path, None).unwrap();
    let chosen = listing
        .entries
        .iter()
        .find(|e| !e.is_directory)
        .unwrap()
        .name
        .clone();
    let out = scratch("select");
    let run = |overwrite| {
        engine::extract(
            ExtractRequest {
                archive: &path,
                out_dir: &out,
                password: None,
                selected: Some(HashSet::from([chosen.clone()])),
                overwrite,
            },
            &ReadCancellation::new(),
            quiet(),
        )
        .unwrap()
    };
    assert_eq!(run(false).written, 1);
    assert_eq!(fs::read_dir(&out).unwrap().flatten().count(), 1);
    fs::write(out.join(&chosen), b"keep me").unwrap();
    let again = run(false);
    assert_eq!((again.written, again.skipped_existing), (0, 1));
    assert_eq!(fs::read(out.join(&chosen)).unwrap(), b"keep me");
    assert_eq!(run(true).written, 1);
    assert_ne!(fs::read(out.join(&chosen)).unwrap(), b"keep me");
}

#[test]
fn cancel_stops_and_leaves_no_staging() {
    let path = fixtures().join("SOLID.RAR");
    let out = scratch("cancel");
    let cancel = ReadCancellation::new();
    let trigger = cancel.clone();
    let quiet: ProgressFn = Arc::new(move |_| trigger.cancel());
    let error = engine::extract(
        ExtractRequest {
            archive: &path,
            out_dir: &out,
            password: None,
            selected: None,
            overwrite: false,
        },
        &cancel,
        quiet,
    )
    .err()
    .unwrap();
    assert_eq!(error.kind, "cancelled");
    assert!(fs::read_dir(&out)
        .unwrap()
        .flatten()
        .all(|e| !e.file_name().to_string_lossy().starts_with(".rawr-tmp")));
}

#[test]
fn symlinks_are_reported_not_created() {
    let path = fixtures().join("symlink.rar");
    let out = scratch("symlink");
    let report = extract_all(&path, &out, None);
    assert!(report.warnings.iter().any(|w| w.contains("link")), "{report:?}");
    for entry in fs::read_dir(&out).unwrap().flatten() {
        assert!(!entry.file_type().unwrap().is_symlink());
    }
}

/// Every archive in a rars checkout: RARS_FIXTURES=<dir> cargo test sweep -- --ignored --nocapture
/// Prints what failed. Many of these are deliberately corrupt, so it's reviewed, not asserted.
#[test]
#[ignore]
fn sweep() {
    let root = PathBuf::from(std::env::var("RARS_FIXTURES").expect("RARS_FIXTURES"));
    let mut stack = vec![root];
    let (mut good, mut bad) = (0, 0);
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("rar")) {
                continue;
            }
            let mut last_error = String::new();
            let outcome = ["", "password", "1234", "test", "Password"].iter().find_map(|p| {
                let password = (!p.is_empty()).then_some(*p);
                let listing = open(&path, password).ok()?;
                let out = scratch("sweep");
                let result = engine::extract(
                    ExtractRequest {
                        archive: &path,
                        out_dir: &out,
                        password,
                        selected: None,
                        overwrite: true,
                    },
                    &ReadCancellation::new(),
                    quiet(),
                );
                match result {
                    Ok(_) => Some((listing, out)),
                    Err(error) => {
                        last_error = error.message;
                        None
                    }
                }
            });
            match outcome {
                Some((listing, out)) => {
                    // Names this app refuses on purpose won't exist on disk.
                    let safe = listing.entries.iter().all(|e| safe_relative(&e.name).is_ok());
                    if safe {
                        assert_sizes_match(&listing, &out);
                    }
                    good += 1;
                }
                None => {
                    let reason = open(&path, None).err().map(|e| e.message);
                    println!("FAILED {} open={reason:?} extract={last_error:?}", path.display());
                    bad += 1;
                }
            }
        }
    }
    println!("sweep: {good} extracted, {bad} failed");
}
