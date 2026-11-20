//! Execution modes: PR events, manual, drift detection.

mod checkout;
pub mod drift;
pub mod manual;
pub mod pr_event;
pub mod run;
#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Runtime environment shared by the modes, built by the CLI.
pub struct Env {
    pub repo_root: PathBuf,
    pub work_root: PathBuf,
    pub max_parallel: Option<usize>,
    pub serialize_init: bool,
    /// Terraform CLI configuration enforcing the provider allowlist, if any.
    pub cli_config: Option<String>,
}

impl Env {
    /// Mutex used to serialise `terraform init` when a plugin cache is in use.
    pub fn init_lock(&self) -> Option<Arc<Mutex<()>>> {
        if self.serialize_init {
            Some(Arc::new(Mutex::new(())))
        } else {
            None
        }
    }
}

/// Process exit codes (see the README).
pub mod exit {
    pub const OK: i32 = 0;
    pub const CONFIG: i32 = 1;
    pub const LOCK_INIT: i32 = 2;
    pub const CONTEXT: i32 = 3;
    pub const INIT: i32 = 4;
    pub const EVENT: i32 = 6;
    pub const COMMAND: i32 = 7;
    pub const JOB_FAILED: i32 = 8;
    pub const NO_CI: i32 = 10;
}
