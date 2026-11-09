//! nikama: backendless Terraform/OpenTofu PR automation for GitHub Actions.
//!
//! See `docs/DESIGN.md` for the design.

// The configuration and result models intentionally carry fields that are
// parsed for compatibility but not read yet.
#![allow(dead_code)]

mod aws_auth;
mod cli;
mod config;
mod engine;
mod github;
mod lock;
mod modes;
mod report;
mod storage;
mod util;

#[tokio::main]
async fn main() {
    let code = cli::run().await;
    std::process::exit(code);
}
