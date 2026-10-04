pub mod cli;
pub mod helper;
pub mod protocol;
pub mod source;

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;

pub mod capture;

pub mod input;
pub mod keymap;
