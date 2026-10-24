//! Text model: piece table ([`buffer`]) and visual row segmentation ([`rows`]).

pub mod buffer;
pub mod rows;

pub use buffer::{Buf, Piece, Swap, TextBuf};
