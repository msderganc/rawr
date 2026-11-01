# rawr

A native Windows app for browsing, testing and extracting RAR archives of every
generation, from DOS-era RAR 1.3 to RAR 7. Written in Rust on the
[rars](https://github.com/bitplane/rars) library, using real Win32 controls via
`native-windows-gui`. No runtime or web view is needed.

## Build

Needs Rust and the Visual Studio C++ build tools.

```bash
cargo build --release
```

The app is the single file `target/release/rawr.exe`.

## Use

Open an archive with the button, by dropping it on the window, or by passing its
path as an argument. Double-click a folder to enter it. Extract applies to the
selected rows, or the whole archive if nothing is selected.

## What it won't write

Entries that could escape the chosen folder or misbehave on Windows are skipped
and listed in the message area: path traversal, alternate data streams, device
names like `CON`, and names differing only by case. Symlinks and junctions are
never created.

Files appear in the destination only after their checksum passes.

## Test

```bash
cargo test
```

To run every sample archive from a rars checkout through extraction:

```bash
RARS_FIXTURES=path/to/rars/crates/rars/tests/fixtures cargo test --release --test engine sweep -- --ignored --nocapture
```

Licensed under Apache-2.0. `src/volumes.rs` and `tests/fixtures` come from rars.
