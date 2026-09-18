mod hooks;
mod native;
pub mod profile;
mod ui;
mod worker;

pub use native::creation_time;
pub use ui::TrainerUi;
pub use ui::WINDOW_ID;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub pid: u32,
    pub name: String,
    pub generation: u64,
    pub created: u64,
}
