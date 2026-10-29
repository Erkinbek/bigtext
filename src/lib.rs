//! BigText core: file access, encodings, line index, piece table, search, replace and save.
//! Nothing in this crate depends on the GUI; `main.rs` hosts the egui front end.

pub mod brackets;
pub mod correlate;
pub mod csv;
pub mod decompress;
pub mod diff;
pub mod encoding;
pub mod file;
pub mod filter;
pub mod find_files;
pub mod headless;
pub mod histogram;
pub mod index;
pub mod jobs;
pub mod json;
pub mod lines;
pub mod loglevel;
pub mod merge;
pub mod replace;
pub mod save;
pub mod split;
pub mod search;
pub mod sysinfo;
pub mod table_export;
pub mod text;
pub mod textops;
pub mod timestamp;
pub mod update;
