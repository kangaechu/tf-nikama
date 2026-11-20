//! nikama: backendless Terraform/OpenTofu PR automation for GitHub Actions.

mod aws;
mod aws_auth;
mod cli;
mod config;
mod engine;
mod gcs;
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
