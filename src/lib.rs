//! BigText core: file access, encodings, line index, piece table, search, replace and save.
//! Nothing in this crate depends on the GUI; `main.rs` hosts the egui front end.

pub mod encoding;
pub mod file;
pub mod index;
pub mod jobs;
pub mod replace;
pub mod save;
pub mod search;
pub mod sysinfo;
pub mod text;
