fn main() -> Result<(), Box<dyn std::error::Error>> {
    let settings = neuromita::settings::StoredSettings {
        revision: 0,
        settings: neuromita::settings::AppSettings::initial_profile(),
    };
    println!("{}", serde_json::to_string_pretty(&settings)?);
    Ok(())
}
