//! PR comment rendering, the single comment writer, and drift notifications.

pub mod comment;
pub mod drift;
#[cfg(test)]
mod tests;
pub mod writer;

pub use comment::{DetailKind, RowStatus, RunReport};
pub use writer::ReportWriter;

/// GitHub's hard limit on issue comment bodies.
pub const COMMENT_MAX_CHARS: usize = 65_536;
