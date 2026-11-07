use std::{error::Error, io::IsTerminal};

use tracing_subscriber::EnvFilter;

pub fn init() -> Result<(), Box<dyn Error + Send + Sync>> {
    let directives = match std::env::var("RUST_LOG") {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => "warn,neuromita=info".to_owned(),
        Err(error) => return Err(error.into()),
    };
    let filter = EnvFilter::builder().with_regex(false).parse(directives)?;
    let ansi = std::io::stderr().is_terminal()
        && std::env::var_os("NO_COLOR").is_none()
        && std::env::var("TERM").as_deref() != Ok("dumb");
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(ansi)
        .with_target(true)
        .with_file(true)
        .with_line_number(true)
        .try_init()?;
    Ok(())
}
