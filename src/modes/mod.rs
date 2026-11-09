//! Execution modes: PR events, manual, drift detection.

pub mod drift;
pub mod manual;
pub mod pr_event;
pub mod run;

/// Process exit codes (DESIGN.md §15.3).
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
