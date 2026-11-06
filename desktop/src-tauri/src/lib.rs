//! Lowkey desktop app (Tauri v2).

pub mod app;
pub mod audio;
pub mod autostart;
pub mod calls;
pub mod config;
pub mod dictation;
pub mod engine;
pub mod export;
pub mod formats;
pub mod history;
pub mod hyprland;
pub mod jobs;
pub mod llm;
pub mod mcp;
pub mod meeting;
pub mod mic;
pub mod microphones;
pub mod models;
pub mod output;
pub mod preview;
pub mod rename;
pub mod screen;
pub mod system_audio;
pub mod tray;
pub mod vocabulary;
pub mod voices;
pub mod whisper_tokens;
#[cfg(parakeet)]
pub mod preview_parakeet;

pub use app::run;
