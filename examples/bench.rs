//! Runs one engine operation, so a script can time it and watch its memory:
//!   bench create <output.rar> <level> <solid:0|1> <input>...
//!   bench extract <archive> <folder>
//!   bench test <archive>
use rars::ReadCancellation;
use rawr::authoring::{self, CreateRequest, Format};
use rawr::engine::{self, ExtractRequest, ProgressFn};
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let quiet: ProgressFn = Arc::new(|_| {});
    let cancel = ReadCancellation::new();
    let outcome = match args[0].as_str() {
        "create" => {
            let inputs: Vec<PathBuf> = args[4..].iter().map(PathBuf::from).collect();
            authoring::create(
                CreateRequest {
                    output: Path::new(&args[1]),
                    inputs: &inputs,
                    format: Format::Rar5,
                    level: args[2].parse().unwrap(),
                    solid: args[3] == "1",
                    password: None,
                    encrypt_names: false,
                    comment: None,
                    recovery_percent: None,
                    volume_bytes: None,
                },
                &cancel,
                quiet,
            )
            .map(|_| ())
        }
        "extract" => engine::extract(
            ExtractRequest {
                archive: Path::new(&args[1]),
                out_dir: Path::new(&args[2]),
                password: None,
                selected: None,
                overwrite: true,
            },
            &cancel,
            quiet,
        )
        .map(|_| ()),
        "test" => engine::test(Path::new(&args[1]), None, &cancel, quiet).map(|_| ()),
        other => panic!("unknown operation {other}"),
    };
    if let Err(error) = outcome {
        eprintln!("{}", error.message);
        std::process::exit(1);
    }
}
