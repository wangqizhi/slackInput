mod hooks;
mod native;
pub mod profile;
mod ui;
mod worker;

pub use native::creation_time;
pub use native::speed::Session as SpeedSession;
pub use ui::TrainerUi;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub pid: u32,
    pub name: String,
    pub generation: u64,
    pub created: u64,
}

mod config;
