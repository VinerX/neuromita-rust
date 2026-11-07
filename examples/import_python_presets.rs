use neuromita::generation::python_import::PythonPresetImport;
use std::{io::Write, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    if arguments.len() != 3 {
        return Err("usage: import_python_presets <api_presets.json> <api_templates.json> <new-output-directory>".into());
    }
    let plan = PythonPresetImport::parse(
        &std::fs::read(&arguments[0])?,
        &std::fs::read(&arguments[1])?,
    )?;
    let catalog = plan.catalog_bytes()?;
    let credentials = plan.credential_bytes()?;
    let templates = serde_json::to_vec_pretty(&plan.templates)?;
    let report = serde_json::to_vec_pretty(&plan.blockers)?;
    let directory = PathBuf::from(&arguments[2]);
    std::fs::create_dir(&directory)?;
    for (name, bytes) in [
        ("presets.json", catalog),
        ("preset-credentials.json", credentials),
        ("python-template-import.json", templates),
        ("python-import-blockers.json", report),
    ] {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(directory.join(name))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    println!(
        "Imported {} presets; {} blocked; {} template descriptors. Source files unchanged.",
        plan.records.len(),
        plan.blockers.len(),
        plan.templates.len()
    );
    for blocker in &plan.blockers {
        println!("Preset {}: {}", blocker.source_id, blocker.reason);
    }
    Ok(())
}
