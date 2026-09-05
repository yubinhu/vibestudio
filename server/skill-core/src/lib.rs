// Transport-agnostic core: skill filesystem ops + discovery, no GUI/Tauri deps.
// Reused by both the Tauri desktop app and the headless skill-server.
pub mod agents;
pub mod commit_agent;
pub mod commitmsg;
pub mod connections;
pub mod discover;
pub mod engine;
pub mod filetypes;
pub mod github;
pub mod gitops;
pub mod gpu;
pub mod keystore;
pub mod mining;
pub mod paths;
pub mod pathsafe;
pub mod preferences;
pub mod process;
pub mod recents;
pub mod remotesync;
pub mod reveal;
pub mod secrets;
pub mod session_title;
pub mod skill;
mod state_store;
pub mod switchboard;
pub mod sync;
pub mod update;
