//! The transcript model of the scrollback interface: which blocks exist,
//! which of their rows have already gone into the terminal's scrollback, and
//! how a streaming answer is cut into whole Markdown blocks that commit while
//! the rest stays in the live region (design §6.2).

pub mod block;
pub mod blocks;
pub mod commit;
pub mod split;
pub mod stream;
pub mod tool_group;
