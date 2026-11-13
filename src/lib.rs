//! BigText core: file access, encodings, line index, piece table, search, replace and save.
//! Nothing in this crate depends on the GUI; `main.rs` hosts the egui front end.

// The byte loops keep `chunks_exact`: with Rust 1.100, `as_chunks` was about 3% slower in
// `encoding::count_byte` (4.72 against 4.87 GB/s) and in the diff's line hash (4.2 against
// 4.3 GB/s) over a 1 GiB buffer.
#![allow(clippy::chunks_exact_to_as_chunks)]

pub mod brackets;
pub mod codec;
pub mod column_edit;
pub mod correlate;
pub mod csv;
pub mod csv_chart;
pub mod csv_check;
pub mod csv_diff;
pub mod csv_expr;
pub mod csv_join;
pub mod csv_pivot;
pub mod csv_rows;
pub mod decompress;
pub mod diff;
pub mod encoding;
pub mod extract;
pub mod extsort;
pub mod fields;
pub mod file;
pub mod filter;
pub mod find_files;
pub mod gaps;
pub mod headless;
pub mod histogram;
pub mod index;
pub mod jobs;
pub mod json;
pub mod json_check;
pub mod json_diff;
pub mod json_fields;
pub mod jsonq;
pub mod lines;
pub mod logformat;
pub mod loglevel;
pub mod logsources;
pub mod merge;
pub mod patch;
pub mod patterns;
pub mod pipe;
pub mod remote;
pub mod replace;
#[cfg(test)]
mod robustness;
pub mod save;
pub mod search;
pub mod setdiff;
pub mod settings_file;
mod spill;
pub mod split;
pub mod sqldump;
pub mod stats;
pub mod sysinfo;
pub mod table_export;
pub mod text;
pub mod textops;
pub mod timestamp;
pub mod unified;
pub mod update;
pub mod watch;
pub mod xml;
