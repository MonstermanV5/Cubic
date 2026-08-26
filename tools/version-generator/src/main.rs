//! Offline validation and deterministic catalog generation for Cubic version data.

use std::{path::PathBuf, process::ExitCode};

use cubic_version::{VersionDataStore, write_catalog};

fn main() -> ExitCode {
    match run(std::env::args_os().skip(1)) {
        Ok(message) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn run(arguments: impl IntoIterator<Item = std::ffi::OsString>) -> Result<String, String> {
    let mut arguments = arguments.into_iter();
    let command = arguments.next().ok_or_else(usage)?;
    let root = arguments.next().map(PathBuf::from).ok_or_else(usage)?;
    if arguments.next().is_some() {
        return Err(usage());
    }
    match command.to_str() {
        Some("validate") => {
            let store = VersionDataStore::open(&root).map_err(|error| error.to_string())?;
            Ok(format!(
                "validated {} installed version dataset(s)",
                store.available_versions().len()
            ))
        }
        Some("build-catalog") => {
            let catalog = write_catalog(&root).map_err(|error| error.to_string())?;
            VersionDataStore::open(&root).map_err(|error| error.to_string())?;
            Ok(format!(
                "wrote deterministic catalog for {} version dataset(s)",
                catalog.entries().len()
            ))
        }
        _ => Err(usage()),
    }
}

fn usage() -> String {
    "Usage:\n  version-generator validate <version-data-root>\n  version-generator build-catalog <version-data-root>".to_owned()
}
