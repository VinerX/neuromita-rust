#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ai_engine;
mod asr;
mod binding;
mod launcher_ui;
mod memory;
mod presenter;
mod releases;
mod runtime;
mod sandbox;
mod settings;
mod speech_status;
mod voice;

mod view {
    slint::include_modules!();
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    binding::run()
}
