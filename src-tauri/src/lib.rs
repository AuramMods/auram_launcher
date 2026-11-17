pub mod auth;
#[cfg(feature = "desktop")]
mod desktop;
pub mod download;
pub mod game;
pub mod jobs;
pub mod pack;
pub mod runtime;
pub mod server;
pub mod storage;
#[cfg(feature = "desktop")]
pub use desktop::run;

#[cfg(feature = "desktop")]
mod updater;
