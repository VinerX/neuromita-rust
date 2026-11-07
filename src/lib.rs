pub mod adapters;
pub mod ai_runtime;
pub mod api;
pub mod app;
pub mod audio;
pub mod character;
pub mod finetune;
pub mod game_context;
pub mod game_link;
pub mod games;
pub mod generation;
mod identity;
mod interaction;
pub mod knowledge;
pub mod launcher;
pub mod lifecycle;
pub mod localization;
pub mod prompts;
// Administrative workflows are contract-tested; an authenticated control route is not yet wired.
#[cfg_attr(not(test), allow(dead_code))]
mod maintenance;
pub mod migration;
pub mod presentation;
pub mod reminders;
pub mod request;
mod runtime;
pub mod server;
pub mod settings;
pub mod timeline;
pub mod voice;

pub use character::memory;
pub use interaction::UserInput;
pub mod turn;

pub mod releases;
