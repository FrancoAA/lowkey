//! open-whisper desktop app (Tauri v2).

pub mod app;
pub mod audio;
pub mod autostart;
pub mod config;
pub mod dictation;
pub mod engine;
pub mod formats;
pub mod hyprland;
pub mod jobs;
pub mod mic;
pub mod models;
pub mod output;
pub mod preview;
#[cfg(parakeet)]
pub mod preview_parakeet;

pub use app::run;
