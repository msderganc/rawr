# Third-party notices

rawr's own code is under the MIT licence in `LICENSE`. These parts aren't.

## Included in this repository

From [rars](https://github.com/bitplane/rars), under the Apache License 2.0
(full text in `LICENSES/Apache-2.0.txt`):

- `src/volumes.rs`, ported from `crates/rars-cli/src/volumes.rs` and modified
  (file name matching now ignores case, and missing parts are reported)
- `tests/fixtures/`, sample archives copied from `crates/rars/tests/fixtures`,
  with some renamed

## Used as libraries

Builds of rawr also contain these, under their own licences:

- [rawrs-engine](https://github.com/msderganc/rawrs-engine), a modified copy of
  [rars](https://github.com/bitplane/rars): Apache-2.0
- [Slint](https://slint.dev): used under the Slint Royalty-free Desktop, Mobile, and
  Web Applications License 2.0, which requires the credit shown in rawr's About screen.
  Anyone distributing rawr needs to keep that credit, or use Slint under another of
  its licences.
- [zip](https://github.com/zip-rs/zip2): MIT
- [sevenz-rust2](https://github.com/hasenbanck/sevenz-rust2): Apache-2.0
- [flate2](https://github.com/rust-lang/flate2-rs): MIT or Apache-2.0
- [drag](https://github.com/crabnebula-dev/drag-rs): Apache-2.0 or MIT
- Their dependencies, listed in `Cargo.lock`
