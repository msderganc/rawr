# rawr

A clean, minimal Windows app for RAR, ZIP and 7z archives, in Rust. It's built on the
[rars](https://github.com/bitplane/rars) library, with a
[Slint](https://slint.dev) interface and no web view or runtime to install.

## What it does

- **Open** every RAR generation, from DOS-era RAR 1.3 to RAR 7, including
  multi-part sets and encrypted archives
- **Open and extract ZIP and 7z** too, including encrypted ones (rawr only creates RAR)
- **Browse** with sorting, search across the whole archive, recent archives, and light
  and dark themes
- **Drag** files out to Explorer, up to 300 MB at a time
- **Extract** everything or a selection, with progress, speed and Cancel
- **Create** RAR 4, 5 or 7 archives, with compression level, password, hidden file
  names, solid mode, split parts, recovery record and comment
- **Change** an archive: add, rename and delete files, and edit its comment
- **Test** integrity, and **repair** archives that have a recovery record
- **Open** a file straight from the archive, with a warning first for programs
- **Explorer**: optional "Extract here", "Add to archive" and `.rar` association, in Settings

Keyboard: Enter opens, Backspace goes up, Delete, F2 to rename, Ctrl+A, Ctrl+F.

## Build

Needs Rust and the Visual Studio C++ build tools.

```bash
cargo build --release
```

The app is the single file `target/release/rawr.exe`, which runs without installing.

For an installer, install [NSIS](https://nsis.sourceforge.io), then:

```bash
makensis /DVERSION=0.3.0 installer\rawr.nsi
```

It installs per-user with no admin prompt. Uninstalling also removes rawr's Explorer
menus and settings, and puts back any previous `.rar` association.

## Good to know

- Editing rebuilds the archive, so it takes about as long as extracting and
  recompressing it. The original is replaced only after the new one is finished.
  Split archives and archives containing links can't be edited.
- Files appear in the destination only after their checksum passes.
- Entries that could escape the chosen folder or misbehave on Windows are skipped
  and listed: path traversal, alternate data streams, device names like `CON`, and
  names differing only by case. Symlinks and junctions are never created.
- Repair reads the whole archive into memory.
- Extracting a selection skips the other files where it can. Solid and split RAR
  archives and 7z still have to decode what comes before the files you chose.
- Multi-part ZIP and 7z sets aren't supported.
- It can't make self-extracting archives.

## Test

```bash
cargo test
```

Run every sample archive from a rars checkout through extraction:

```bash
RARS_FIXTURES=path/to/rars/crates/rars/tests/fixtures cargo test --release --test engine sweep -- --ignored --nocapture
```

Check that WinRAR accepts the archives rawr writes:

```bash
UNRAR="C:\Program Files\WinRAR\UnRAR.exe" cargo test --release --test authoring winrar -- --ignored --nocapture
```

## Licences

rawr is Apache-2.0. `src/volumes.rs` and `tests/fixtures` come from rars. Slint is
used under its royalty-free licence, which requires the credit in the About screen.
