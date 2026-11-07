use std::{error::Error, path::Path};

use neuromita::localization::{FallbackPolicy, Localizer, source_template_json};

fn main() -> Result<(), Box<dyn Error>> {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    match arguments.first().and_then(|value| value.to_str()) {
        Some("template") if arguments.len() == 1 => println!("{}", source_template_json()?),
        Some("check") if arguments.len() <= 2 => {
            let mut localizer = Localizer::bundled(FallbackPolicy::EnglishThenSource)?;
            if let Some(directory) = arguments.get(1) {
                localizer.reload_directory(Path::new(directory))?;
            }
            println!(
                "Validated {} language catalogs",
                localizer.languages().len()
            );
        }
        _ => return Err("usage: neuromita-i18n template | check [directory]".into()),
    }
    Ok(())
}
