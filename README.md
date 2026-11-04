<img src="assets/rawr-icon.png" alt="" width="96">

# rawr

A clean, minimal Windows app for RAR, ZIP, 7z and gzip archives, written in Rust. It's a
single exe with a [Slint](https://slint.dev) interface: no web view, no runtime to
install, and no nag screen.

Its RAR support comes from [rawrs-engine](https://github.com/msderganc/rawrs-engine), a
copy of the [rars](https://github.com/bitplane/rars) library reworked for speed and
memory. On the benchmark below, rawr is faster than WinRAR on 57 of 60 operations.

## What it does

- **Opens** every RAR generation, from DOS-era RAR 1.3 to RAR 7, including multi-part
  sets and encrypted archives
- **Opens and extracts** ZIP, 7z and gzip too, including encrypted ZIP and 7z
- **Creates** RAR 4, 5 or 7 archives, with compression level, password, hidden file
  names, solid mode, split parts, recovery record and comment
- **Changes** an archive in place: add, rename and delete files, and edit its comment
- **Extracts** everything or a selection, with progress, speed and Cancel
- **Browses** with sorting, search across the whole archive, recent archives, and light
  and dark themes
- **Drags** files out to Explorer, up to 300 MB at a time
- **Tests** integrity, and **repairs** archives that have a recovery record
- **Opens** a file straight from the archive, warning first if it's a program
- **Explorer integration**, optional in Settings: "Extract here" and "Add to archive"
  menus, and a shortcut to make rawr your default

Keyboard: Enter opens, Backspace goes up, Delete deletes, F2 renames, Ctrl+A selects all,
Ctrl+F searches.

## Install

Download `rawr-<version>-setup.exe` from
[Releases](https://github.com/msderganc/rawr/releases) and run it. It installs for your
user only, with no admin prompt, and adds rawr to "Open with" and Default apps for `.rar`,
`.zip`, `.7z`, `.gz` and `.tgz` without taking over your current default. Uninstalling
removes all of that, along with rawr's Explorer menus and settings.

Or download `rawr.exe` on its own: it runs from anywhere without installing.

## Performance

rawr against WinRAR 7.23's command-line `Rar.exe` and `UnRAR.exe`, both making RAR 5
archives, measured with [`scripts/bench.ps1`](scripts/bench.ps1). A score of 100 matches
WinRAR and higher is better: 150 means WinRAR takes 1.5 times as long, or as much memory.

| Score | rawr | What it measures |
|---|---:|---|
| **Overall** | **126** | 70% speed, 20% resources, 10% size |
| Speed | 143 | Wall time, faster on 57 of 60 operations |
| Resources | 92 | Peak memory 82, CPU time 104 |
| Size | 97 | Archive size on the create operations |

| | Speed | Peak memory | CPU time | Size |
|---|---:|---:|---:|---:|
| Creating (40 operations) | 135 | 83 | 114 | 97 |
| Extracting (10) | 158 | 71 | 74 | |
| Testing (10) | 159 | 92 | 102 | |

Put simply, rawr needs about 70% of WinRAR's time overall, and under two-thirds to
extract or test. The three operations where it isn't faster are within 0.01 seconds.
What it gives up is memory: creating archives from many files can take two to three
times as much, and extracting buffers more so it can write files on background threads.
Its archives come out about 3% larger.

<details>
<summary>How it was measured</summary>

- Five data sets: 96 mixed files (384 MB); a Node.js install together with a Rust source
  tree (113 MB); the Rust source tree alone (473 files, 11 MB); one 89 MB executable; and
  64 MB of random data
- Twelve operations on each: creating at Fast, levels 2, 4 and 5, and Normal; creating
  solid at Fast, level 5 and Normal; then extracting and testing a plain and a solid
  archive. Both sides read the same archives, made by WinRAR.
- Best of 3 runs, scored as geometric means. Every archive rawr made was checked with
  UnRAR.
- Intel Core i9-12900K, 64 GB, Windows 11, November 2026
- The script drives rawr's engine through [`examples/bench.rs`](examples/bench.rs), the
  same code the app runs, without the window

To run it yourself, with WinRAR installed:

```powershell
pwsh scripts/bench.ps1 -Path <folder or file>[,<another>] [-Runs 3] [-Csv results.csv]
```

</details>

## Good to know

- Editing rebuilds the archive, so it takes about as long as extracting and
  recompressing it. The original is replaced only after the new one is finished.
  Split archives and archives containing links can't be edited.
- Files appear in the destination only after their checksum passes.
- Entries that could escape the chosen folder or misbehave on Windows are skipped and
  listed: path traversal, alternate data streams, device names like `CON`, and names
  that differ only by case. Symlinks and junctions are never created.
- Repair reads the whole archive into memory.
- The default compression level is Fast, for new archives and edits. Many separate files
  compress faster than one large one, because they spread across more threads.
- Extracting a selection skips the other files where it can. Solid and split RAR archives
  and 7z still have to decode everything before the files you chose.
- Multi-part ZIP and 7z sets aren't supported.
- A `.tar.gz` extracts to its `.tar`, without unpacking further. gzip can't state its full
  size reliably, so that shows as unknown and progress follows the compressed data.
- rawr can't make self-extracting archives.

## Build

Needs Rust and the Visual Studio C++ build tools.

```bash
cargo build --release
```

The app is the single file `target/release/rawr.exe`.

For the installer, install [NSIS](https://nsis.sourceforge.io), then:

```bash
makensis /DVERSION=0.4.0 installer\rawr.nsi
```

## Test

```bash
cargo test
```

Run every sample archive from a rawrs-engine checkout through extraction:

```bash
RARS_FIXTURES=path/to/rawrs-engine/crates/rars/tests/fixtures cargo test --release --test engine sweep -- --ignored --nocapture
```

Check that another RAR decoder accepts the archives rawr writes:

```bash
RAR_DECODER=path/to/decoder cargo test --release --test authoring external_decoder -- --ignored --nocapture
```

## Licences

rawr is MIT. `src/volumes.rs` and `tests/fixtures` come from rars and stay under
Apache-2.0, as does rawrs-engine. Slint is used under its royalty-free licence, which
requires the credit in the About screen. See [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).
