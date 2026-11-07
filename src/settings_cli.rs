use std::{error::Error, ffi::OsString, io, path::PathBuf};

use neuromita::settings::{JsonSettingsRepository, SettingsImportFormat};

pub const HELP: &str = "neuromita settings export --source STORE.json --destination BACKUP.json\n\
    neuromita settings import --source BACKUP.json --destination STORE.json [--format rust|python]\n\n\
    Offline operations: no Core and no GEMINI_API_KEY. Stop the application first.\n\
    Only the current strict Rust dev settings format is accepted; no dev migrations.\n\
    The default format is rust. Python migration is an unimplemented extension point.\n\
    Preset bodies, credentials and Timeline are not included.\n";

pub fn run(arguments: Vec<OsString>) -> Result<(), Box<dyn Error>> {
    let mut arguments = arguments.into_iter();
    let command = arguments
        .next()
        .ok_or_else(|| invalid("expected export or import; use --help"))?;
    if command == "--help" {
        print!("{HELP}");
        return Ok(());
    }
    let import = match command.to_str() {
        Some("export") => false,
        Some("import") => true,
        _ => return Err(invalid("expected export or import; use --help").into()),
    };
    let mut source = None;
    let mut destination = None;
    let mut format = None;
    while let Some(option) = arguments.next() {
        if option == "--help" {
            print!("{HELP}");
            return Ok(());
        }
        match option.to_str() {
            Some("--source") => path_argument(&mut source, arguments.next())?,
            Some("--destination") => path_argument(&mut destination, arguments.next())?,
            Some("--format") if import => {
                let value = match arguments.next().as_deref().and_then(|value| value.to_str()) {
                    Some("rust") => SettingsImportFormat::Rust,
                    Some("python") => SettingsImportFormat::Python,
                    _ => return Err(invalid("--format requires rust or python").into()),
                };
                if format.replace(value).is_some() {
                    return Err(invalid("duplicate --format").into());
                }
            }
            _ => return Err(invalid("unknown settings option; use --help").into()),
        }
    }
    let source = source.ok_or_else(|| invalid("--source is required"))?;
    let destination = destination.ok_or_else(|| invalid("--destination is required"))?;
    let transfer = if import {
        let bytes = std::fs::read(&source)?;
        JsonSettingsRepository::new(destination)
            .import_from(format.unwrap_or(SettingsImportFormat::Rust), &bytes)?
    } else {
        JsonSettingsRepository::new(source).export_to(&destination)?
    };
    println!(
        "{} full Rust AppSettings (revision {})",
        if import { "Imported" } else { "Exported" },
        transfer.stored().revision
    );
    Ok(())
}

fn path_argument(slot: &mut Option<PathBuf>, value: Option<OsString>) -> io::Result<()> {
    let value = value
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid("option requires a path"))?;
    if slot.replace(PathBuf::from(value)).is_some() {
        return Err(invalid("duplicate path option"));
    }
    Ok(())
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
