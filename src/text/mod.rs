//! Text model: piece table ([`buffer`], with its piece list in [`tree`] and its add buffer in
//! [`addbuf`]) and visual row segmentation ([`rows`]).

mod addbuf;
pub mod buffer;
pub mod rows;
mod tree;

pub use buffer::{Buf, Piece, Swap, TextBuf};
