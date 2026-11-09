//! GitHub integration: Actions context, REST client, event → job conversion.

pub mod client;
pub mod comment_cmd;
pub mod context;
pub mod events;
#[cfg(test)]
mod tests;

pub use client::{GithubClient, PrInfo};
pub use comment_cmd::{parse_comment, CommentVerb};
pub use context::GithubContext;
