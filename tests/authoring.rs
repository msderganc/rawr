use rars::ReadCancellation;
use rawr::authoring::{self, CreateRequest, Edit, Format};
use rawr::engine::{self, ExtractRequest, OpenedArchive, ProgressFn};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rawr-author-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn quiet() -> ProgressFn {
    Arc::new(|_| {})
}

fn open(path: &Path, password: Option<&str>) -> Result<OpenedArchive, engine::AppError> {
    engine::open(path, password, &ReadCancellation::new())
}

fn extract_all(path: &Path, out: &Path, password: Option<&str>) {
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
    .unwrap();
}

fn sample_tree(name: &str) -> PathBuf {
    let root = scratch(name);
    let src = root.join("input");
    fs::create_dir_all(src.join("sub/empty")).unwrap();
    fs::write(src.join("a.txt"), b"alpha alpha alpha alpha").unwrap();
    fs::write(src.join("sub/b.txt"), vec![7u8; 200_000]).unwrap();
    fs::write(src.join("sub/日本語.txt"), b"unicode").unwrap();
    root
}

fn request<'a>(output: &'a Path, inputs: &'a [PathBuf], format: Format) -> CreateRequest<'a> {
    CreateRequest {
        output,
        inputs,
        format,
        level: 3,
        solid: false,
        password: None,
        encrypt_names: false,
        comment: None,
        recovery_percent: None,
        volume_bytes: None,
    }
}

fn names(path: &Path, password: Option<&str>) -> Vec<String> {
    open(path, password)
        .unwrap()
        .entries
        .into_iter()
        .map(|e| e.name)
        .collect()
}

#[test]
fn created_archives_round_trip_in_each_format() {
    for format in [Format::Rar4, Format::Rar5, Format::Rar7] {
        let root = sample_tree(&format!("create-{format:?}"));
        let output = root.join("out.rar");
        let inputs = [root.join("input")];
        authoring::create(
            request(&output, &inputs, format),
            &ReadCancellation::new(),
            quiet(),
        )
        .unwrap();
        let listing = open(&output, None).unwrap();
        assert!(
            listing
                .entries
                .iter()
                .any(|e| e.name == "input/sub/empty" && e.is_directory),
            "{format:?}"
        );
        assert!(
            listing.entries.iter().all(|e| e.modified_ms.is_some()),
            "{format:?} lost dates"
        );
        let out = root.join("x");
        extract_all(&output, &out, None);
        assert_eq!(
            fs::read(out.join("input/sub/b.txt")).unwrap(),
            vec![7u8; 200_000]
        );
        assert_eq!(
            fs::read(out.join("input/sub/日本語.txt")).unwrap(),
            b"unicode"
        );
        assert!(out.join("input/sub/empty").is_dir());
    }
}

#[test]
fn password_hidden_names_and_parts() {
    let root = sample_tree("options");
    let inputs = [root.join("input")];
    let cancel = ReadCancellation::new();

    let locked = root.join("locked.rar");
    let mut options = request(&locked, &inputs, Format::Rar5);
    options.password = Some("secret");
    options.encrypt_names = true;
    options.solid = true;
    authoring::create(options, &cancel, quiet()).unwrap();
    assert_eq!(open(&locked, None).unwrap_err().kind, "password_required");
    assert!(engine::test(&locked, Some("secret"), &cancel, quiet()).unwrap() > 0);

    let base = root.join("set.rar");
    let mut options = request(&base, &inputs, Format::Rar5);
    options.level = 0;
    options.volume_bytes = Some(70_000);
    let report = authoring::create(options, &cancel, quiet()).unwrap();
    assert!(report.parts.len() >= 3, "{:?}", report.parts);
    assert!(report.parts[0].ends_with("set.part01.rar"));
    assert!(engine::test(&report.parts[1], None, &cancel, quiet()).unwrap() > 0);
    assert!(fs::read_dir(&root)
        .unwrap()
        .flatten()
        .all(|e| !e.file_name().to_string_lossy().ends_with(".tmp")));
}

#[test]
fn repair_restores_a_damaged_archive() {
    let root = sample_tree("repair");
    let inputs = [root.join("input")];
    let cancel = ReadCancellation::new();
    let guarded = root.join("guarded.rar");
    let mut options = request(&guarded, &inputs, Format::Rar5);
    options.recovery_percent = Some(10);
    options.level = 0;
    authoring::create(options, &cancel, quiet()).unwrap();

    let clean = root.join("clean.rar");
    assert_eq!(
        authoring::repair(&guarded, &clean, None, &cancel).unwrap(),
        "No damage found."
    );

    let mut bytes = fs::read(&guarded).unwrap();
    let middle = bytes.len() / 3;
    bytes[middle..middle + 64]
        .iter_mut()
        .for_each(|b| *b ^= 0xA5);
    fs::write(&guarded, bytes).unwrap();
    assert!(engine::test(&guarded, None, &cancel, quiet()).is_err());
    let fixed = root.join("fixed.rar");
    let outcome = authoring::repair(&guarded, &fixed, None, &cancel).unwrap();
    assert!(outcome.contains("damaged data"), "{outcome}");
    assert!(engine::test(&fixed, None, &cancel, quiet()).unwrap() > 0);
}

#[test]
fn repair_without_a_record_says_so() {
    let root = sample_tree("no-record");
    let inputs = [root.join("input")];
    let cancel = ReadCancellation::new();
    let plain = root.join("plain.rar");
    authoring::create(request(&plain, &inputs, Format::Rar5), &cancel, quiet()).unwrap();
    let fixed = root.join("fixed.rar");
    assert!(authoring::repair(&plain, &fixed, None, &cancel).is_err());
    assert!(!fixed.exists());
}

#[test]
fn edits_keep_everything_else_intact() {
    let root = sample_tree("edit");
    let inputs = [root.join("input")];
    let cancel = ReadCancellation::new();
    let path = root.join("e.rar");
    let mut options = request(&path, &inputs, Format::Rar5);
    options.password = Some("pw");
    options.comment = Some("keep this");
    authoring::create(options, &cancel, quiet()).unwrap();

    let edit = |change| authoring::edit(&path, Some("pw"), change, &cancel, quiet());
    edit(Edit::Rename {
        from: "input/sub".into(),
        to: "input/renamed".into(),
    })
    .unwrap();
    let listed = names(&path, Some("pw"));
    assert!(listed.contains(&"input/renamed/b.txt".to_owned()));
    assert!(!listed.iter().any(|n| n.starts_with("input/sub")));

    edit(Edit::Delete(HashSet::from(["input/a.txt".to_owned()]))).unwrap();
    assert!(!names(&path, Some("pw")).contains(&"input/a.txt".to_owned()));

    let extra = root.join("extra.txt");
    fs::write(&extra, b"new").unwrap();
    edit(Edit::Add {
        inputs: vec![extra.clone()],
        into: "input/".into(),
    })
    .unwrap();
    fs::write(&extra, b"replaced").unwrap();
    edit(Edit::Add {
        inputs: vec![extra],
        into: "input/".into(),
    })
    .unwrap();

    let listing = open(&path, Some("pw")).unwrap();
    assert!(listing.any_encrypted);
    assert_eq!(listing.comment.as_deref(), Some("keep this"));
    assert_eq!(
        listing
            .entries
            .iter()
            .filter(|e| e.name == "input/extra.txt")
            .count(),
        1
    );
    let out = root.join("x");
    extract_all(&path, &out, Some("pw"));
    assert_eq!(fs::read(out.join("input/extra.txt")).unwrap(), b"replaced");
    assert_eq!(
        fs::read(out.join("input/renamed/b.txt")).unwrap(),
        vec![7u8; 200_000]
    );
    assert!(fs::read_dir(&root)
        .unwrap()
        .flatten()
        .all(|e| !e.file_name().to_string_lossy().starts_with(".rawr")));

    assert!(edit(Edit::Rename {
        from: "input/extra.txt".into(),
        to: "../evil".into(),
    })
    .is_err());
}

#[test]
fn edit_keeps_dates_and_cancel_leaves_the_original() {
    let root = sample_tree("edit-dates");
    let inputs = [root.join("input")];
    let cancel = ReadCancellation::new();
    let path = root.join("e.rar");
    authoring::create(request(&path, &inputs, Format::Rar5), &cancel, quiet()).unwrap();
    let date_of = |name: &str| {
        open(&path, None)
            .unwrap()
            .entries
            .into_iter()
            .find(|e| e.name == name)
            .unwrap()
            .modified_ms
    };
    let before = date_of("input/a.txt");
    assert!(before.is_some());
    authoring::edit(
        &path,
        None,
        Edit::Delete(HashSet::from(["input/sub/b.txt".to_owned()])),
        &cancel,
        quiet(),
    )
    .unwrap();
    assert_eq!(date_of("input/a.txt"), before);

    let original = fs::read(&path).unwrap();
    let stop = ReadCancellation::new();
    let trigger = stop.clone();
    let cancelling: ProgressFn = Arc::new(move |_| trigger.cancel());
    let result = authoring::edit(
        &path,
        None,
        Edit::Delete(HashSet::from(["input/a.txt".to_owned()])),
        &stop,
        cancelling,
    );
    assert_eq!(result.err().unwrap().kind, "cancelled");
    assert_eq!(fs::read(&path).unwrap(), original);
}

/// Checks real WinRAR accepts what we write:
/// UNRAR="C:\Program Files\WinRAR\UnRAR.exe" cargo test winrar -- --ignored --nocapture
#[test]
#[ignore]
fn winrar_accepts_our_archives() {
    let unrar = std::env::var("UNRAR").expect("UNRAR");
    let root = sample_tree("winrar");
    let inputs = [root.join("input")];
    let cancel = ReadCancellation::new();
    let check = |path: &Path, password: Option<&str>| {
        let mut command = std::process::Command::new(&unrar);
        command.arg("t").arg("-idq");
        command.arg(format!("-p{}", password.unwrap_or("-")));
        let status = command.arg(path).status().unwrap();
        println!("{}: {status}", path.file_name().unwrap().to_string_lossy());
        assert!(status.success(), "{}", path.display());
    };
    for (name, format) in [
        ("rar4.rar", Format::Rar4),
        ("rar5.rar", Format::Rar5),
        ("rar7.rar", Format::Rar7),
    ] {
        let path = root.join(name);
        authoring::create(request(&path, &inputs, format), &cancel, quiet()).unwrap();
        check(&path, None);
    }
    let path = root.join("locked.rar");
    let mut options = request(&path, &inputs, Format::Rar5);
    options.password = Some("pw");
    options.encrypt_names = true;
    options.solid = true;
    authoring::create(options, &cancel, quiet()).unwrap();
    check(&path, Some("pw"));
    let path = root.join("guarded.rar");
    let mut options = request(&path, &inputs, Format::Rar5);
    options.recovery_percent = Some(5);
    options.comment = Some("hello");
    authoring::create(options, &cancel, quiet()).unwrap();
    check(&path, None);
    let edited = root.join("edited.rar");
    authoring::create(request(&edited, &inputs, Format::Rar5), &cancel, quiet()).unwrap();
    authoring::edit(
        &edited,
        None,
        Edit::Delete(HashSet::from(["input/a.txt".to_owned()])),
        &cancel,
        quiet(),
    )
    .unwrap();
    check(&edited, None);
    let base = root.join("set.rar");
    let mut options = request(&base, &inputs, Format::Rar5);
    options.level = 0;
    options.volume_bytes = Some(70_000);
    let report = authoring::create(options, &cancel, quiet()).unwrap();
    check(&report.parts[0], None);
}
#[test]
fn comments_can_be_changed_and_removed() {
    let root = sample_tree("comment");
    let inputs = [root.join("input")];
    let cancel = ReadCancellation::new();
    let path = root.join("c.rar");
    authoring::create(request(&path, &inputs, Format::Rar5), &cancel, quiet()).unwrap();
    let set = |text: &str| {
        authoring::edit(&path, None, Edit::SetComment(text.into()), &cancel, quiet()).unwrap();
    };
    set("first");
    assert_eq!(open(&path, None).unwrap().comment.as_deref(), Some("first"));
    set("second, 日本語");
    assert_eq!(
        open(&path, None).unwrap().comment.as_deref(),
        Some("second, 日本語")
    );
    set("");
    assert_eq!(open(&path, None).unwrap().comment, None);
    assert_eq!(names(&path, None).len(), 6);
}

#[test]
fn selecting_a_file_skips_the_others() {
    let root = scratch("skip");
    let src = root.join("input");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("keep.txt"), b"wanted").unwrap();
    fs::write(src.join("other.bin"), vec![0x5Au8; 50_000]).unwrap();
    let path = root.join("s.rar");
    let inputs = [src.clone()];
    let mut options = request(&path, &inputs, Format::Rar5);
    options.level = 0;
    authoring::create(options, &ReadCancellation::new(), quiet()).unwrap();

    // Damage the middle of the unselected file. If selection really skips it,
    // extracting the other file can't notice.
    let mut bytes = fs::read(&path).unwrap();
    let at = bytes.windows(64).position(|w| w == [0x5Au8; 64]).unwrap() + 20_000;
    bytes[at] ^= 0xFF;
    fs::write(&path, bytes).unwrap();

    let run = |selected: Option<HashSet<String>>, out: &Path| {
        engine::extract(
            ExtractRequest {
                archive: &path,
                out_dir: out,
                password: None,
                selected,
                overwrite: false,
            },
            &ReadCancellation::new(),
            quiet(),
        )
    };
    let out = root.join("one");
    let report = run(Some(HashSet::from(["input/keep.txt".to_owned()])), &out).unwrap();
    assert_eq!(report.written, 1);
    assert_eq!(fs::read(out.join("input/keep.txt")).unwrap(), b"wanted");
    assert!(!out.join("input/other.bin").exists());
    assert!(run(None, &root.join("all")).is_err());
}

#[test]
fn selecting_from_a_solid_archive_still_works() {
    let root = sample_tree("solid-select");
    let inputs = [root.join("input")];
    let path = root.join("s.rar");
    let mut options = request(&path, &inputs, Format::Rar5);
    options.solid = true;
    authoring::create(options, &ReadCancellation::new(), quiet()).unwrap();
    let out = root.join("x");
    let report = engine::extract(
        ExtractRequest {
            archive: &path,
            out_dir: &out,
            password: None,
            selected: Some(HashSet::from(["input/sub/b.txt".to_owned()])),
            overwrite: false,
        },
        &ReadCancellation::new(),
        quiet(),
    )
    .unwrap();
    assert_eq!(report.written, 1);
    assert_eq!(
        fs::read(out.join("input/sub/b.txt")).unwrap(),
        vec![7u8; 200_000]
    );
}

#[test]
fn many_files_unpack_side_by_side_and_damage_is_still_caught() {
    let dir = scratch("side-by-side");
    let source = dir.join("source");
    fs::create_dir_all(source.join("inner")).unwrap();
    let mut inputs = Vec::new();
    for index in 0..40u32 {
        let folder = if index % 2 == 0 {
            source.join("inner")
        } else {
            source.clone()
        };
        let path = folder.join(format!("file{index}.bin"));
        let data: Vec<u8> = (0..50_000u32)
            .map(|at| (at.wrapping_mul(index + 7) >> 3) as u8)
            .collect();
        fs::write(&path, data).unwrap();
        inputs.push(path);
    }
    let path = dir.join("many.rar");
    let cancel = ReadCancellation::new();
    authoring::create(request(&path, &inputs, Format::Rar5), &cancel, quiet()).unwrap();

    assert_eq!(engine::test(&path, None, &cancel, quiet()).unwrap(), 40);
    let out = dir.join("out");
    extract_all(&path, &out, None);
    for input in &inputs {
        let unpacked = out.join(input.file_name().unwrap());
        assert_eq!(fs::read(unpacked).unwrap(), fs::read(input).unwrap());
    }
    assert!(!fs::read_dir(&out).unwrap().any(|entry| entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .contains(".rawr-tmp")));

    let mut bytes = fs::read(&path).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0xff;
    fs::write(&path, bytes).unwrap();
    assert!(engine::test(&path, None, &cancel, quiet()).is_err());
    let damaged = dir.join("damaged");
    assert!(engine::extract(
        ExtractRequest {
            archive: &path,
            out_dir: &damaged,
            password: None,
            selected: None,
            overwrite: true,
        },
        &cancel,
        quiet(),
    )
    .is_err());
    assert!(!fs::read_dir(&damaged).unwrap().any(|entry| entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .contains(".rawr-tmp")));
}
