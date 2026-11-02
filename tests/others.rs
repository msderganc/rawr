use rars::ReadCancellation;
use rawr::engine::{self, ExtractRequest, ProgressFn};
use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use zip::unstable::write::FileOptionsExt;
use zip::write::SimpleFileOptions;
use zip::{AesMode, ZipWriter};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rawr-other-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn quiet() -> ProgressFn {
    Arc::new(|_| {})
}

fn extract(
    path: &Path,
    out: &Path,
    password: Option<&str>,
    selected: Option<HashSet<String>>,
) -> Result<engine::ExtractReport, engine::AppError> {
    engine::extract(
        ExtractRequest {
            archive: path,
            out_dir: out,
            password,
            selected,
            overwrite: false,
        },
        &ReadCancellation::new(),
        quiet(),
    )
}

fn make_zip(path: &Path, files: &[(&str, &[u8])], aes: Option<&str>, legacy: Option<&[u8]>) {
    let mut writer = ZipWriter::new(fs::File::create(path).unwrap());
    for (name, data) in files {
        let mut options = SimpleFileOptions::default();
        if let Some(password) = aes {
            options = options.with_aes_encryption(AesMode::Aes256, password);
        }
        if let Some(password) = legacy {
            options = options.with_deprecated_encryption(password).unwrap();
        }
        writer.start_file(*name, options).unwrap();
        writer.write_all(data).unwrap();
    }
    writer.finish().unwrap();
}

#[test]
fn zip_lists_extracts_and_tests() {
    let root = scratch("zip");
    let path = root.join("a.zip");
    let big = vec![9u8; 300_000];
    make_zip(
        &path,
        &[
            ("dir/one.txt", b"one"),
            ("dir\\two.bin", &big),
            ("日本語.txt", b"uni"),
        ],
        None,
        None,
    );
    let listing = engine::open(&path, None, &ReadCancellation::new()).unwrap();
    assert_eq!(listing.format, "ZIP");
    assert!(listing.entries.iter().any(|e| e.name == "dir/two.bin"));
    assert!(listing.entries.iter().all(|e| e.modified_ms.is_some()));
    assert_eq!(
        engine::test(&path, None, &ReadCancellation::new(), quiet()).unwrap(),
        3
    );
    let out = root.join("x");
    assert_eq!(extract(&path, &out, None, None).unwrap().written, 3);
    assert_eq!(fs::read(out.join("dir/two.bin")).unwrap(), big);
    assert_eq!(fs::read(out.join("日本語.txt")).unwrap(), b"uni");

    let some = root.join("some");
    let chosen = HashSet::from(["dir/one.txt".to_owned()]);
    assert_eq!(
        extract(&path, &some, None, Some(chosen)).unwrap().written,
        1
    );
    assert!(!some.join("dir/two.bin").exists());

    let misnamed = root.join("really-a-zip.rar");
    fs::copy(&path, &misnamed).unwrap();
    assert_eq!(
        engine::open(&misnamed, None, &ReadCancellation::new())
            .unwrap()
            .format,
        "ZIP"
    );
}

#[test]
fn zip_never_writes_outside_the_folder() {
    let root = scratch("zip-slip");
    let path = root.join("evil.zip");
    make_zip(
        &path,
        &[
            ("../escaped.txt", b"bad"),
            ("..\\escaped2.txt", b"bad"),
            ("C:/escaped3.txt", b"bad"),
            ("a.txt:stream", b"bad"),
            ("CON", b"bad"),
            ("ok.txt", b"good"),
        ],
        None,
        None,
    );
    let out = root.join("deep").join("x");
    let report = extract(&path, &out, None, None).unwrap();
    assert_eq!(report.written, 1);
    assert_eq!(report.warnings.len(), 5, "{:?}", report.warnings);
    assert!(!root.join("deep/escaped.txt").exists());
    assert!(!root.join("deep/escaped2.txt").exists());
    assert_eq!(fs::read(out.join("ok.txt")).unwrap(), b"good");
}

#[test]
fn zip_damage_is_noticed_and_nothing_is_published() {
    let root = scratch("zip-damage");
    let path = root.join("a.zip");
    let mut options_data = vec![0u8; 50_000];
    options_data
        .iter_mut()
        .enumerate()
        .for_each(|(i, b)| *b = (i * 7 % 251) as u8);
    let mut writer = ZipWriter::new(fs::File::create(&path).unwrap());
    let stored = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    writer.start_file("data.bin", stored).unwrap();
    writer.write_all(&options_data).unwrap();
    writer.finish().unwrap();
    let mut bytes = fs::read(&path).unwrap();
    bytes[20_000] ^= 0xFF;
    fs::write(&path, bytes).unwrap();
    let out = root.join("x");
    assert!(extract(&path, &out, None, None).is_err());
    assert!(!out.join("data.bin").exists());
    assert!(fs::read_dir(&out)
        .unwrap()
        .flatten()
        .all(|e| !e.file_name().to_string_lossy().starts_with(".rawr")));
}

#[test]
fn zip_passwords() {
    let root = scratch("zip-pw");
    for (name, aes, legacy) in [
        ("aes.zip", Some("secret"), None),
        ("old.zip", None, Some(b"secret".as_slice())),
    ] {
        let path = root.join(name);
        make_zip(&path, &[("s.txt", b"hidden text here")], aes, legacy);
        assert!(
            engine::open(&path, None, &ReadCancellation::new())
                .unwrap()
                .any_encrypted,
            "{name}"
        );
        let out = root.join(format!("{name}-none"));
        assert_eq!(
            extract(&path, &out, None, None).unwrap_err().kind,
            "password_required",
            "{name}"
        );
        let out = root.join(format!("{name}-wrong"));
        let wrong = extract(&path, &out, Some("nope"), None).unwrap_err();
        assert_ne!(wrong.kind, "cancelled");
        assert!(!out.join("s.txt").exists(), "{name}");
        let out = root.join(format!("{name}-right"));
        extract(&path, &out, Some("secret"), None).unwrap();
        assert_eq!(fs::read(out.join("s.txt")).unwrap(), b"hidden text here");
    }
}

fn sample_tree(root: &Path) -> PathBuf {
    let src = root.join("src");
    fs::create_dir_all(src.join("inner/empty")).unwrap();
    fs::write(src.join("a.txt"), b"alpha").unwrap();
    fs::write(src.join("zero.txt"), b"").unwrap();
    fs::write(src.join("inner/b.bin"), vec![3u8; 400_000]).unwrap();
    fs::write(src.join("inner/日本語.txt"), b"uni").unwrap();
    src
}

#[test]
fn seven_lists_extracts_and_tests() {
    let root = scratch("7z");
    let src = sample_tree(&root);
    let path = root.join("a.7z");
    sevenz_rust2::compress_to_path(&src, &path).unwrap();
    let listing = engine::open(&path, None, &ReadCancellation::new()).unwrap();
    assert_eq!(listing.format, "7z");
    assert!(listing.entries.iter().any(|e| e.name == "inner/b.bin"));
    assert!(engine::test(&path, None, &ReadCancellation::new(), quiet()).unwrap() >= 3);
    let out = root.join("x");
    extract(&path, &out, None, None).unwrap();
    assert_eq!(
        fs::read(out.join("inner/b.bin")).unwrap(),
        vec![3u8; 400_000]
    );
    assert_eq!(fs::read(out.join("inner/日本語.txt")).unwrap(), b"uni");
    assert_eq!(fs::read(out.join("zero.txt")).unwrap(), b"");
    let listed = listing.entries.iter().any(|e| e.name == "inner/empty");
    println!("empty folder in listing: {listed}");
    assert_eq!(out.join("inner/empty").is_dir(), listed);

    let some = root.join("some");
    let chosen = HashSet::from(["a.txt".to_owned()]);
    assert_eq!(
        extract(&path, &some, None, Some(chosen)).unwrap().written,
        1
    );
    assert!(!some.join("inner/b.bin").exists());
}

#[test]
fn seven_passwords_and_damage() {
    let root = scratch("7z-pw");
    let src = sample_tree(&root);
    let path = root.join("locked.7z");
    sevenz_rust2::compress_to_path_encrypted(&src, &path, "secret".into()).unwrap();

    let needs = engine::open(&path, None, &ReadCancellation::new())
        .and_then(|_| extract(&path, &root.join("none"), None, None).map(|_| ()))
        .unwrap_err();
    assert_eq!(needs.kind, "password_required", "{}", needs.message);
    let wrong = engine::open(&path, Some("nope"), &ReadCancellation::new())
        .and_then(|_| extract(&path, &root.join("wrong"), Some("nope"), None).map(|_| ()))
        .unwrap_err();
    assert_eq!(wrong.kind, "bad_password", "{}", wrong.message);
    assert!(!root.join("wrong/a.txt").exists());
    let out = root.join("right");
    extract(&path, &out, Some("secret"), None).unwrap();
    assert_eq!(fs::read(out.join("a.txt")).unwrap(), b"alpha");

    let plain = root.join("plain.7z");
    sevenz_rust2::compress_to_path(&src, &plain).unwrap();
    let mut bytes = fs::read(&plain).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle..middle + 16]
        .iter_mut()
        .for_each(|b| *b ^= 0xA5);
    fs::write(&plain, bytes).unwrap();
    assert!(engine::test(&plain, None, &ReadCancellation::new(), quiet()).is_err());
}

#[test]
fn cancel_stops_zip_and_cleans_up() {
    let root = scratch("zip-cancel");
    let path = root.join("a.zip");
    let big = vec![1u8; 2_000_000];
    make_zip(&path, &[("one.bin", &big), ("two.bin", &big)], None, None);
    let cancel = ReadCancellation::new();
    let trigger = cancel.clone();
    let stopping: ProgressFn = Arc::new(move |_| trigger.cancel());
    let out = root.join("x");
    let failure = engine::extract(
        ExtractRequest {
            archive: &path,
            out_dir: &out,
            password: None,
            selected: None,
            overwrite: false,
        },
        &cancel,
        stopping,
    )
    .err()
    .unwrap();
    assert_eq!(failure.kind, "cancelled");
    assert!(fs::read_dir(&out)
        .unwrap()
        .flatten()
        .all(|e| !e.file_name().to_string_lossy().starts_with(".rawr")));
}

fn gzip(path: &Path, members: &[&[u8]]) {
    let mut file = fs::File::create(path).unwrap();
    for data in members {
        let mut encoder = flate2::write::GzEncoder::new(&mut file, flate2::Compression::fast());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap();
    }
}

#[test]
fn gzip_lists_tests_and_extracts() {
    let root = scratch("gz");
    let path = root.join("rates.json.gz");
    let body = vec![b'x'; 3_000_000];
    gzip(&path, &[&body]);
    let listing = engine::open(&path, None, &ReadCancellation::new()).unwrap();
    assert_eq!(listing.format, "gzip");
    assert!(listing.size_unknown);
    assert_eq!(listing.entries.len(), 1);
    assert_eq!(listing.entries[0].name, "rates.json");
    assert_eq!(
        engine::test(&path, None, &ReadCancellation::new(), quiet()).unwrap(),
        1
    );
    let out = root.join("x");
    assert_eq!(extract(&path, &out, None, None).unwrap().written, 1);
    assert_eq!(fs::read(out.join("rates.json")).unwrap(), body);
}

#[test]
fn gzip_joined_streams_and_tar_names() {
    let root = scratch("gz-multi");
    let path = root.join("backup.tgz");
    gzip(&path, &[b"first ", b"second"]);
    let out = root.join("x");
    extract(&path, &out, None, None).unwrap();
    assert_eq!(fs::read(out.join("backup.tar")).unwrap(), b"first second");
}

#[test]
fn gzip_damage_and_truncation_publish_nothing() {
    let root = scratch("gz-bad");
    let path = root.join("d.gz");
    let body: Vec<u8> = (0..2_000_000u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 13) as u8)
        .collect();
    gzip(&path, &[&body]);
    let good = fs::read(&path).unwrap();
    let mut flipped = good.clone();
    let middle = flipped.len() / 2;
    flipped[middle] ^= 0xFF;
    for (name, bytes) in [
        ("flipped", flipped),
        ("cut", good[..good.len() / 2].to_vec()),
    ] {
        fs::write(&path, bytes).unwrap();
        assert!(
            engine::test(&path, None, &ReadCancellation::new(), quiet()).is_err(),
            "{name}"
        );
        let out = root.join(name);
        assert!(extract(&path, &out, None, None).is_err(), "{name}");
        assert!(!out.join("d").exists(), "{name}");
        assert!(fs::read_dir(&out)
            .unwrap()
            .flatten()
            .all(|e| !e.file_name().to_string_lossy().starts_with(".rawr")));
    }
}

#[test]
fn gzip_cancel_stops_and_cleans_up() {
    let root = scratch("gz-cancel");
    let path = root.join("c.gz");
    gzip(&path, &[&vec![1u8; 20_000_000]]);
    let cancel = ReadCancellation::new();
    let trigger = cancel.clone();
    let stopping: ProgressFn = Arc::new(move |_| trigger.cancel());
    let out = root.join("x");
    let failure = engine::extract(
        ExtractRequest {
            archive: &path,
            out_dir: &out,
            password: None,
            selected: None,
            overwrite: false,
        },
        &cancel,
        stopping,
    )
    .err()
    .unwrap();
    assert_eq!(failure.kind, "cancelled");
    assert!(!out.join("c").exists());
}

#[test]
fn unknown_files_are_reported_as_unsupported() {
    let root = scratch("unknown");
    let path = root.join("notes.txt");
    fs::write(&path, b"just some text, long enough to read").unwrap();
    assert_eq!(
        engine::open(&path, None, &ReadCancellation::new())
            .unwrap_err()
            .kind,
        "unsupported"
    );
}

/// Checks a real file end to end, without writing anything:
/// GZ_FILE=path cargo test --release --test others real_gzip -- --ignored --nocapture
#[test]
#[ignore]
fn real_gzip_file_verifies() {
    let path = PathBuf::from(std::env::var("GZ_FILE").expect("GZ_FILE"));
    let started = std::time::Instant::now();
    assert_eq!(
        engine::test(&path, None, &ReadCancellation::new(), quiet()).unwrap(),
        1
    );
    println!("verified in {:?}", started.elapsed());
}
