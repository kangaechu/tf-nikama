//! Orchestration tests: apply gating, summaries, dependency-level execution.

use super::pr_event::{apply_gate, check_required_statuses, summarize, ApplyGate};
use super::run::{run_by_levels, JobResult, Outcome, RunContext};
use crate::config::load_from_str;
use crate::engine::{Command, Job, PlanResult, PlanSummary};
use crate::github::client::{CombinedStatus, StatusEntry};
use crate::github::PrInfo;
use crate::storage::NoopPlanStorage;
use std::sync::Arc;

fn pr(merged: bool, state: &str, mergeable: Option<bool>, mergeable_state: Option<&str>) -> PrInfo {
    PrInfo {
        number: 1,
        head_ref: "f".into(),
        head_sha: "s".into(),
        base_ref: "main".into(),
        merged,
        state: state.into(),
        draft: false,
        mergeable,
        mergeable_state: mergeable_state.map(|s| s.to_string()),
    }
}

fn apply_jobs(skip: bool) -> Vec<Job> {
    let tmp = tempfile::tempdir().unwrap();
    let c = load_from_str(
        &format!(
            "projects:\n  - name: a\n    dir: a\n    workflow: w\nworkflows:\n  w:\n    workflow_configuration:\n      skip_merge_check: {}\n",
            skip
        ),
        tmp.path(),
    )
    .unwrap();
    let p = &c.projects[0];
    vec![Job::new(
        p,
        c.workflow_for(p),
        Command::Apply,
        Some(1),
        "x",
        "me",
        "o/r",
        "",
        "",
    )]
}

#[test]
fn apply_gate_decisions() {
    let jobs = apply_jobs(false);
    assert_eq!(
        apply_gate(&pr(true, "closed", None, None), &jobs),
        ApplyGate::Allowed
    );
    assert_eq!(
        apply_gate(&pr(false, "open", Some(true), Some("clean")), &jobs),
        ApplyGate::Allowed
    );
    assert_eq!(
        apply_gate(&pr(false, "open", Some(true), Some("unstable")), &jobs),
        ApplyGate::Allowed
    );
    assert_eq!(
        apply_gate(&pr(false, "open", Some(true), Some("has_hooks")), &jobs),
        ApplyGate::Allowed
    );
    assert_eq!(
        apply_gate(&pr(false, "open", Some(false), Some("blocked")), &jobs),
        ApplyGate::CheckStatuses
    );
    assert_eq!(
        apply_gate(&pr(false, "closed", Some(true), Some("clean")), &jobs),
        ApplyGate::Denied("pull request is closed".into())
    );
    assert!(
        matches!(apply_gate(&pr(false, "open", Some(false), Some("dirty")), &jobs), ApplyGate::Denied(m) if m.contains("not mergeable"))
    );
    assert!(
        matches!(apply_gate(&pr(false, "open", None, None), &jobs), ApplyGate::Denied(m) if m.contains("not computed"))
    );
    assert!(
        matches!(apply_gate(&pr(false, "open", Some(true), Some("behind")), &jobs), ApplyGate::Denied(m) if m.contains("behind"))
    );
    // skip_merge_check bypasses everything.
    assert_eq!(
        apply_gate(
            &pr(false, "open", Some(false), Some("dirty")),
            &apply_jobs(true)
        ),
        ApplyGate::Allowed
    );
}

#[test]
fn required_statuses_ignore_own_contexts() {
    let combined = CombinedStatus {
        state: "pending".into(),
        statuses: vec![
            StatusEntry {
                context: "digger/apply".into(),
                state: "pending".into(),
            },
            StatusEntry {
                context: "digger/plan".into(),
                state: "failure".into(),
            },
            StatusEntry {
                context: "nikama/plan/x".into(),
                state: "pending".into(),
            },
            StatusEntry {
                context: "ci/test".into(),
                state: "success".into(),
            },
        ],
    };
    assert!(check_required_statuses(&combined, "digger/apply").is_ok());
    let blocked = CombinedStatus {
        state: "pending".into(),
        statuses: vec![StatusEntry {
            context: "ci/lint".into(),
            state: "pending".into(),
        }],
    };
    let err = check_required_statuses(&blocked, "digger/apply").unwrap_err();
    assert!(err.contains("ci/lint (pending)"), "{}", err);
}

fn plan_result(empty: bool) -> JobResult {
    JobResult {
        project: "p".into(),
        command: Command::Plan,
        outcome: Ok(Outcome::Plan(PlanResult {
            summary: PlanSummary {
                create: if empty { 0 } else { 1 },
                ..Default::default()
            },
            plan_text: String::new(),
            plan_json: String::new(),
            plan_file: std::path::PathBuf::new(),
            additional_output: None,
            steps: vec![],
        })),
    }
}

#[test]
fn summarize_counts() {
    let results = vec![
        plan_result(true),
        plan_result(false),
        JobResult {
            project: "q".into(),
            command: Command::Plan,
            outcome: Err("x".into()),
        },
    ];
    assert_eq!(
        summarize(&results, false),
        "2 succeeded, 1 with changes, 1 failed"
    );
    assert_eq!(
        summarize(&results[..1], true),
        "1 succeeded, some projects locked by other PRs"
    );
}

#[tokio::test]
async fn run_by_levels_skips_dependents_of_failed_jobs() {
    let tmp = tempfile::tempdir().unwrap();
    let config = Arc::new(
        load_from_str(
            "projects:\n  - name: a\n    dir: missing/a\n  - name: b\n    dir: missing/b\n    depends_on: [a]\n  - name: c\n    dir: missing/c\n",
            tmp.path(),
        )
        .unwrap(),
    );
    let jobs: Vec<Job> = config
        .projects
        .iter()
        .map(|p| {
            Job::new(
                p,
                config.workflow_for(p),
                Command::Apply,
                Some(1),
                "x",
                "me",
                "o/r",
                "",
                "",
            )
        })
        .collect();
    let ctx = Arc::new(RunContext {
        config: config.clone(),
        repo_root: tmp.path().to_path_buf(),
        work_root: tmp.path().join("work"),
        init_lock: None,
        storage: Arc::new(NoopPlanStorage),
        locker: None,
        writer: None,
        max_parallel: 2,
    });
    let mut results = run_by_levels(ctx, jobs, 2).await;
    results.sort_by(|x, y| x.project.cmp(&y.project));
    assert_eq!(results.len(), 3);
    // `a` and `c` fail because their directories do not exist; `b` is skipped.
    assert!(
        results[0].outcome.is_err(),
        "a should fail: {:?}",
        results[0]
    );
    assert!(
        matches!(&results[1].outcome, Ok(Outcome::Skipped(reason)) if reason.contains("dependency a failed"))
    );
    assert!(results[2].outcome.is_err());
}
