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
pub mod meeting;
pub mod mic;
pub mod models;
pub mod output;
pub mod preview;
pub mod system_audio;
#[cfg(parakeet)]
pub mod preview_parakeet;

pub use app::run;
