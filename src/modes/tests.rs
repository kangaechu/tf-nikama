//! Orchestration tests: apply gating, summaries, dependency-level execution.

use super::pr_event::{
    acquire_locks, all_plans_empty, apply_gate, check_approved, check_blocked, check_undiverged,
    closed_pr_rejection, draft_pr_rejection, no_lock_reason, release_no_change_locks,
    required_checks, set_noop_statuses, summarize, sweep_locks, ApplyGate, PlanLocking,
};
use super::run::{run_by_levels, JobResult, Outcome, RunContext};
use crate::config::load_from_str;
use crate::engine::{Command, Job, PlanResult, PlanSummary};
use crate::github::client::{CheckContext, CheckKind, CompareInfo, MergeRequirements, ReviewInfo};
use crate::github::PrInfo;
use crate::storage::{NoopPlanStorage, PlanMeta, PlanStorage, StoredPlan};
use std::sync::Arc;

fn pr(merged: bool, state: &str, mergeable: Option<bool>, mergeable_state: Option<&str>) -> PrInfo {
    PrInfo {
        number: 1,
        head_ref: "f".into(),
        head_sha: "s".into(),
        merge_commit_sha: None,
        base_ref: "main".into(),
        merged,
        state: state.into(),
        draft: false,
        mergeable,
        mergeable_state: mergeable_state.map(|s| s.to_string()),
        author: "author".into(),
        labels: vec![],
    }
}

fn apply_jobs(skip: bool) -> Vec<Job> {
    apply_jobs_with(skip, None)
}

/// Apply job for one project; `requirements` is the raw `apply_requirements`
/// list (`None` keeps the default).
fn apply_jobs_with(skip: bool, requirements: Option<&str>) -> Vec<Job> {
    let tmp = tempfile::tempdir().unwrap();
    let req = requirements
        .map(|r| format!("    apply_requirements: {}\n", r))
        .unwrap_or_default();
    let c = load_from_str(
        &format!(
            "projects:\n  - name: a\n    dir: a\n    workflow: w\n{}workflows:\n  w:\n    workflow_configuration:\n      skip_merge_check: {}\n",
            req, skip
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
    // skip_merge_check bypasses the mergeable check on an open PR ...
    assert_eq!(
        apply_gate(
            &pr(false, "open", Some(false), Some("dirty")),
            &apply_jobs(true)
        ),
        ApplyGate::Allowed
    );
    // ... but never the closed-PR check.
    assert_eq!(
        apply_gate(
            &pr(false, "closed", Some(true), Some("clean")),
            &apply_jobs(true)
        ),
        ApplyGate::Denied("pull request is closed".into())
    );
}

#[test]
fn apply_gate_without_mergeable_requirement() {
    // `approved` alone no longer disables gating: mergeable is not checked,
    // but the closed-PR check stays and `approved` is reported as required.
    let jobs = apply_jobs_with(false, Some("[approved]"));
    assert_eq!(
        apply_gate(&pr(false, "open", Some(false), Some("dirty")), &jobs),
        ApplyGate::Allowed
    );
    assert_eq!(
        apply_gate(&pr(false, "closed", None, None), &jobs),
        ApplyGate::Denied("pull request is closed".into())
    );
    let req = required_checks(&jobs);
    assert!(req.contains("approved"));
    assert!(!req.contains("mergeable"));
}

#[test]
fn required_checks_collects_requirements() {
    let req = required_checks(&apply_jobs_with(
        false,
        Some("[approved, mergeable, undiverged]"),
    ));
    assert_eq!(
        req.into_iter().collect::<Vec<_>>(),
        vec!["approved", "mergeable", "undiverged"]
    );
    // skip_merge_check removes only `mergeable`.
    let req = required_checks(&apply_jobs_with(true, Some("[mergeable, undiverged]")));
    assert_eq!(req.into_iter().collect::<Vec<_>>(), vec!["undiverged"]);
    // Non-apply jobs contribute nothing.
    let mut jobs = apply_jobs(false);
    jobs[0].command = Command::Plan;
    assert!(required_checks(&jobs).is_empty());
}

fn review(user: &str, state: &str) -> ReviewInfo {
    ReviewInfo {
        user: user.into(),
        state: state.into(),
    }
}

#[test]
fn approved_requirement() {
    // No reviews at all.
    assert!(check_approved(&[], "author").is_err());
    // Comments only.
    assert!(check_approved(&[review("bob", "COMMENTED")], "author").is_err());
    // One approval.
    assert!(check_approved(&[review("bob", "APPROVED")], "author").is_ok());
    // The author's own approval does not count.
    assert!(check_approved(&[review("author", "APPROVED")], "author").is_err());
    // Changes requested after an approval by the same reviewer block.
    let err = check_approved(
        &[
            review("bob", "APPROVED"),
            review("bob", "CHANGES_REQUESTED"),
        ],
        "author",
    )
    .unwrap_err();
    assert!(err.contains("changes requested by @bob"), "{}", err);
    // Approval after changes requested by the same reviewer resolves it.
    assert!(check_approved(
        &[
            review("bob", "CHANGES_REQUESTED"),
            review("bob", "APPROVED")
        ],
        "author"
    )
    .is_ok());
    // Another reviewer's outstanding request blocks even with an approval.
    assert!(check_approved(
        &[
            review("bob", "APPROVED"),
            review("carol", "CHANGES_REQUESTED")
        ],
        "author"
    )
    .is_err());
    // A dismissed approval no longer counts.
    assert!(check_approved(
        &[review("bob", "APPROVED"), review("bob", "DISMISSED")],
        "author"
    )
    .is_err());
    // A later comment does not revoke an approval.
    assert!(check_approved(
        &[review("bob", "APPROVED"), review("bob", "COMMENTED")],
        "author"
    )
    .is_ok());
}

#[test]
fn reviews_from_users_without_write_access_are_ignored() {
    use super::pr_event::trusted_reviews;
    let reviews = [
        review("bob", "APPROVED"),
        review("eve", "APPROVED"),
        review("eve", "CHANGES_REQUESTED"),
        review("mallory", "CHANGES_REQUESTED"),
    ];
    let (kept, ignored) = trusted_reviews(&reviews, |u| u == "bob");
    assert_eq!(kept, vec![review("bob", "APPROVED")]);
    assert_eq!(ignored, vec!["eve", "mallory"]);
    // The sock-puppet approval alone does not satisfy the requirement ...
    let (kept, _) = trusted_reviews(&reviews[1..2], |_| false);
    assert!(check_approved(&kept, "author").is_err());
    // ... and a read-only user's "changes requested" does not block either.
    let (kept, _) = trusted_reviews(&reviews, |u| u == "bob");
    assert!(check_approved(&kept, "author").is_ok());
}

#[test]
fn undiverged_requirement() {
    assert!(check_undiverged(&CompareInfo {
        ahead_by: 3,
        behind_by: 0
    })
    .is_ok());
    let err = check_undiverged(&CompareInfo {
        ahead_by: 3,
        behind_by: 2,
    })
    .unwrap_err();
    assert!(err.contains("2 commit(s) behind"), "{}", err);
}

fn ctx(name: &str, kind: CheckKind, required: bool, state: &str, url: &str) -> CheckContext {
    CheckContext {
        name: name.into(),
        kind,
        required,
        state: state.into(),
        details_url: url.into(),
    }
}

#[test]
fn blocked_pr_passes_when_only_nikama_blocks_it() {
    let req = MergeRequirements {
        review_decision: Some("APPROVED".into()),
        contexts: vec![
            // Our own statuses, whatever their state.
            ctx("digger/apply", CheckKind::Status, true, "pending", ""),
            ctx("digger/plan", CheckKind::Status, true, "failure", ""),
            ctx("nikama/plan/x", CheckKind::Status, true, "pending", ""),
            // The job of the current workflow run (the apply itself).
            ctx(
                "nikama",
                CheckKind::CheckRun,
                true,
                "pending",
                "https://github.com/o/r/actions/runs/777/job/1",
            ),
            // Required and green, in the accepted conclusions.
            ctx("ci/test", CheckKind::Status, true, "success", ""),
            ctx(
                "lint",
                CheckKind::CheckRun,
                true,
                "neutral",
                "https://github.com/o/r/actions/runs/1/job/2",
            ),
            ctx("docs", CheckKind::CheckRun, true, "skipped", ""),
            // Not required: ignored even when red.
            ctx("optional", CheckKind::CheckRun, false, "failure", ""),
        ],
    };
    assert!(check_blocked(&req, Some("777")).is_ok());
    // No review rule at all.
    let none = MergeRequirements {
        review_decision: None,
        contexts: vec![],
    };
    assert!(check_blocked(&none, None).is_ok());
}

#[test]
fn blocked_pr_is_denied_for_reviews_and_required_checks() {
    let req = MergeRequirements {
        review_decision: Some("REVIEW_REQUIRED".into()),
        contexts: vec![
            ctx("ci/lint", CheckKind::Status, true, "pending", ""),
            ctx(
                "build",
                CheckKind::CheckRun,
                true,
                "failure",
                "https://github.com/o/r/actions/runs/1/job/2",
            ),
            ctx(
                "slow",
                CheckKind::CheckRun,
                true,
                "pending",
                "https://github.com/o/r/actions/runs/2/job/3",
            ),
        ],
    };
    let err = check_blocked(&req, Some("777")).unwrap_err();
    assert!(err.contains("required reviews are missing"), "{}", err);
    assert!(err.contains("ci/lint (pending)"), "{}", err);
    assert!(err.contains("build (failure)"), "{}", err);
    assert!(err.contains("slow (pending)"), "{}", err);

    let changes = MergeRequirements {
        review_decision: Some("CHANGES_REQUESTED".into()),
        contexts: vec![],
    };
    let err = check_blocked(&changes, None).unwrap_err();
    assert!(err.contains("changes were requested"), "{}", err);

    // Without a run id nothing is treated as our own job: a pending
    // required check run is a blocker.
    let own_job = MergeRequirements {
        review_decision: None,
        contexts: vec![ctx(
            "nikama",
            CheckKind::CheckRun,
            true,
            "pending",
            "https://github.com/o/r/actions/runs/777/job/1",
        )],
    };
    assert!(check_blocked(&own_job, None).is_err());
    assert!(check_blocked(&own_job, Some("777")).is_ok());
    assert!(check_blocked(&own_job, Some("778")).is_err());

    // Another job of the same workflow run that has already failed is a
    // real blocker: only unfinished ones are treated as "us".
    let sibling = MergeRequirements {
        review_decision: None,
        contexts: vec![ctx(
            "policy",
            CheckKind::CheckRun,
            true,
            "failure",
            "https://github.com/o/r/actions/runs/777/job/9",
        )],
    };
    let err = check_blocked(&sibling, Some("777")).unwrap_err();
    assert!(err.contains("policy (failure)"), "{}", err);
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

/// Applies four projects whose directories do not exist, so every job that
/// actually runs fails: `a` and `c` in level 0, `b` (depends on `a`) in
/// level 1, `d` (depends on `b`) in level 2. Returns `(project, outcome)`
/// where the outcome is `Err` for a job that ran and failed, or the skip
/// reason.
async fn run_levels_with(on_failure: &str) -> Vec<(String, Result<String, ()>)> {
    let tmp = tempfile::tempdir().unwrap();
    let config = Arc::new(
        load_from_str(
            &format!(
                "dependency_configuration:\n  on_failure: {}\nprojects:\n  - name: a\n    dir: missing/a\n  - name: b\n    dir: missing/b\n    depends_on: [a]\n  - name: c\n    dir: missing/c\n  - name: d\n    dir: missing/d\n    depends_on: [b]\n",
                on_failure
            ),
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
        head_sha: None,
        cli_config: None,
    });
    let mut results = run_by_levels(ctx, jobs, 2).await;
    results.sort_by(|x, y| x.project.cmp(&y.project));
    assert_eq!(results.len(), 4);
    results
        .into_iter()
        .map(|r| {
            let outcome = match r.outcome {
                Ok(Outcome::Skipped(reason)) => Ok(reason),
                Ok(other) => panic!("{} unexpectedly succeeded: {:?}", r.project, other),
                Err(_) => Err(()),
            };
            (r.project, outcome)
        })
        .collect()
}

#[tokio::test]
async fn run_by_levels_skips_dependents_of_failed_jobs() {
    let results = run_levels_with("skip_dependents").await;
    // `a` and `c` run and fail; `b` is skipped because `a` failed and `d`
    // because `b` was skipped, while the unrelated `c` still runs.
    assert_eq!(
        results,
        vec![
            ("a".to_string(), Err(())),
            ("b".to_string(), Ok("dependency a failed".to_string())),
            ("c".to_string(), Err(())),
            ("d".to_string(), Ok("dependency b failed".to_string())),
        ]
    );
}

#[tokio::test]
async fn run_by_levels_abort_skips_every_later_level() {
    let results = run_levels_with("abort").await;
    assert_eq!(results[0], ("a".to_string(), Err(())));
    assert_eq!(results[2], ("c".to_string(), Err(())));
    // Level 0 runs to completion; `b` and `d` are skipped, naming whichever
    // level-0 project was recorded as failing first.
    for (name, outcome) in [&results[1], &results[3]] {
        let reason = outcome
            .as_ref()
            .unwrap_or_else(|_| panic!("{} should be skipped", name));
        assert!(
            reason == "run aborted because a failed" || reason == "run aborted because c failed",
            "{}: {}",
            name,
            reason
        );
    }
}

#[tokio::test]
async fn run_by_levels_continue_runs_dependents_anyway() {
    let results = run_levels_with("continue").await;
    assert!(
        results.iter().all(|(_, outcome)| outcome.is_err()),
        "every project should have run: {:?}",
        results
    );
}

/// In-memory plan storage that is configured (not noop). Empty by default.
#[derive(Default)]
struct MemoryStorage {
    plans: std::sync::Mutex<std::collections::HashMap<String, StoredPlan>>,
}

impl MemoryStorage {
    fn with(key: &str, head_sha: Option<&str>) -> Self {
        Self::with_flag(key, head_sha, false)
    }

    fn with_flag(key: &str, head_sha: Option<&str>, no_changes: bool) -> Self {
        let s = Self::default();
        s.plans.lock().unwrap().insert(
            key.to_string(),
            StoredPlan {
                bytes: b"not-a-real-plan".to_vec(),
                head_sha: head_sha.map(|h| h.to_string()),
                extra_args: vec![],
                no_changes,
            },
        );
        s
    }
}

#[async_trait::async_trait]
impl PlanStorage for MemoryStorage {
    async fn store(&self, key: &str, bytes: &[u8], meta: &PlanMeta) -> anyhow::Result<()> {
        self.plans.lock().unwrap().insert(
            key.to_string(),
            StoredPlan {
                bytes: bytes.to_vec(),
                head_sha: meta.head_sha.clone(),
                extra_args: meta.extra_args.clone(),
                no_changes: meta.no_changes,
            },
        );
        Ok(())
    }
    async fn retrieve(&self, key: &str) -> anyhow::Result<Option<StoredPlan>> {
        Ok(self.plans.lock().unwrap().get(key).cloned())
    }
    async fn delete(&self, key: &str) -> anyhow::Result<()> {
        self.plans.lock().unwrap().remove(key);
        Ok(())
    }
    async fn exists(&self, key: &str) -> anyhow::Result<bool> {
        Ok(self.plans.lock().unwrap().contains_key(key))
    }
    fn name(&self) -> &'static str {
        "memory"
    }
}

fn run_ctx(
    tmp: &tempfile::TempDir,
    config: &Arc<crate::config::Config>,
    storage: Arc<dyn PlanStorage>,
    head_sha: Option<&str>,
) -> RunContext {
    RunContext {
        config: config.clone(),
        repo_root: tmp.path().to_path_buf(),
        work_root: tmp.path().join("work"),
        init_lock: None,
        storage,
        locker: None,
        writer: None,
        max_parallel: 1,
        head_sha: head_sha.map(|h| h.to_string()),
        cli_config: None,
    }
}

fn demo_fixture(dir_exists: bool) -> (tempfile::TempDir, Arc<crate::config::Config>) {
    let tmp = tempfile::tempdir().unwrap();
    if dir_exists {
        std::fs::create_dir_all(tmp.path().join("infra/demo")).unwrap();
        std::fs::write(
            tmp.path().join("infra/demo/main.tf"),
            "resource \"terraform_data\" \"demo\" {\n  input = \"hello\"\n}\n",
        )
        .unwrap();
    }
    let config = Arc::new(
        load_from_str(
            "projects:\n  - name: demo\n    dir: infra/demo\n",
            tmp.path(),
        )
        .unwrap(),
    );
    (tmp, config)
}

fn demo_apply_job(config: &crate::config::Config) -> Job {
    let p = &config.projects[0];
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
}

#[tokio::test]
async fn apply_fails_when_storage_has_no_plan() {
    let (tmp, config) = demo_fixture(false);
    let ctx = run_ctx(
        &tmp,
        &config,
        Arc::new(MemoryStorage::default()),
        Some("abc"),
    );
    let result = super::run::run_job(&ctx, demo_apply_job(&config)).await;
    let err = result.outcome.unwrap_err();
    assert!(err.contains("no stored plan found"), "{}", err);
    assert!(err.contains("o-r-1-demo.tfplan"), "{}", err);
    assert!(err.contains("run `nikama plan` first"), "{}", err);
}

#[tokio::test]
async fn apply_without_storage_plans_first_and_reports_it() {
    if std::process::Command::new("terraform")
        .arg("version")
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        eprintln!("skipping: terraform not found on PATH");
        return;
    }
    let (tmp, config) = demo_fixture(true);
    let ctx = run_ctx(&tmp, &config, Arc::new(NoopPlanStorage), None);
    let result = super::run::run_job(&ctx, demo_apply_job(&config)).await;
    let Ok(Outcome::Apply(a)) = result.outcome else {
        panic!("expected apply outcome: {:?}", result.outcome);
    };
    let pre = a.pre_plan.expect("plan produced before apply");
    assert_eq!(pre.summary.create, 1);
    assert!(pre.plan_file.exists());
    assert!(a.output.contains("Apply complete!"), "{}", a.output);
    assert_eq!(a.summary.map(|s| s.added), Some(1));
}

#[test]
fn stored_plan_commit_check() {
    use super::run::check_stored_plan_commit;
    // Same commit: fine.
    assert!(check_stored_plan_commit("p", Some("abc"), Some("abc"), "nikama").is_ok());
    // Different commit: refused, with shortened SHAs.
    let err = check_stored_plan_commit(
        "p",
        Some("0123456789abcdef0123456789abcdef01234567"),
        Some("fedcba9876543210fedcba9876543210fedcba98"),
        "nikama",
    )
    .unwrap_err();
    assert!(err.contains("from commit 0123456789ab"), "{}", err);
    assert!(err.contains("head is fedcba987654"), "{}", err);
    assert!(err.contains("run `nikama plan` first"), "{}", err);
    // Legacy plan without metadata: accepted.
    assert!(check_stored_plan_commit("p", None, Some("abc"), "nikama").is_ok());
    // Outside PR mode nothing to compare against.
    assert!(check_stored_plan_commit("p", Some("abc"), None, "nikama").is_ok());
}

#[tokio::test]
async fn apply_refuses_stored_plan_from_another_commit() {
    let (tmp, config) = demo_fixture(false);
    let storage = Arc::new(MemoryStorage::with("o-r-1-demo.tfplan", Some("old-sha")));
    let ctx = run_ctx(&tmp, &config, storage.clone(), Some("new-sha"));
    let result = super::run::run_job(&ctx, demo_apply_job(&config)).await;
    let err = result.outcome.unwrap_err();
    assert!(
        err.contains("stored plan is from commit old-sha"),
        "{}",
        err
    );
    assert!(err.contains("head is new-sha"), "{}", err);
    // The stale plan is kept so the user can inspect it; nothing was applied.
    assert!(storage.exists("o-r-1-demo.tfplan").await.unwrap());
}

#[tokio::test]
async fn apply_skips_stored_plan_without_changes() {
    // The fixture has no Terraform code, so any attempt to run Terraform
    // would fail: the job must finish without touching it.
    let (tmp, config) = demo_fixture(false);
    let storage = Arc::new(MemoryStorage::with_flag(
        "o-r-1-demo.tfplan",
        Some("abc"),
        true,
    ));
    let ctx = run_ctx(&tmp, &config, storage.clone(), Some("abc"));
    let result = super::run::run_job(&ctx, demo_apply_job(&config)).await;
    assert!(
        matches!(
            result.outcome,
            Ok(Outcome::ApplyNoChanges { plan: None, .. })
        ),
        "{:?}",
        result.outcome
    );
    // The plan is consumed like an applied one.
    assert!(!storage.exists("o-r-1-demo.tfplan").await.unwrap());
}

#[tokio::test]
async fn apply_still_checks_the_commit_of_a_plan_without_changes() {
    let (tmp, config) = demo_fixture(false);
    let storage = Arc::new(MemoryStorage::with_flag(
        "o-r-1-demo.tfplan",
        Some("old-sha"),
        true,
    ));
    let ctx = run_ctx(&tmp, &config, storage.clone(), Some("new-sha"));
    let result = super::run::run_job(&ctx, demo_apply_job(&config)).await;
    let err = result.outcome.unwrap_err();
    assert!(
        err.contains("stored plan is from commit old-sha"),
        "{}",
        err
    );
    assert!(storage.exists("o-r-1-demo.tfplan").await.unwrap());
}

#[tokio::test]
async fn apply_accepts_stored_plan_without_commit_metadata() {
    // No terraform in the fixture dir, so the job fails later at `init`;
    // what matters is that the commit check did not reject it.
    let (tmp, config) = demo_fixture(false);
    let storage = Arc::new(MemoryStorage::with("o-r-1-demo.tfplan", None));
    let ctx = run_ctx(&tmp, &config, storage, Some("new-sha"));
    let result = super::run::run_job(&ctx, demo_apply_job(&config)).await;
    let err = result.outcome.unwrap_err();
    assert!(!err.contains("stored plan is from commit"), "{}", err);
    assert!(!err.contains("no stored plan found"), "{}", err);
}

#[tokio::test]
async fn plan_stores_head_sha_with_the_plan_file() {
    if std::process::Command::new("terraform")
        .arg("version")
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        eprintln!("skipping: terraform not found on PATH");
        return;
    }
    let (tmp, config) = demo_fixture(true);
    let storage = Arc::new(MemoryStorage::default());
    let ctx = run_ctx(&tmp, &config, storage.clone(), Some("head-123"));
    let mut job = demo_apply_job(&config);
    job.command = Command::Plan;
    let result = super::run::run_job(&ctx, job).await;
    assert!(
        matches!(result.outcome, Ok(Outcome::Plan(_))),
        "{:?}",
        result.outcome
    );
    let stored = storage
        .retrieve("o-r-1-demo.tfplan")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.head_sha.as_deref(), Some("head-123"));
    assert!(!stored.bytes.is_empty());
    assert!(!stored.no_changes);
}

#[tokio::test]
async fn a_targeted_stored_plan_without_changes_is_not_a_full_apply() {
    let (tmp, config) = demo_fixture(true);
    let storage = Arc::new(MemoryStorage::default());
    storage
        .store(
            "o-r-1-demo.tfplan",
            b"stored",
            &crate::storage::PlanMeta {
                head_sha: Some("head-123".into()),
                extra_args: vec!["-target=module.x".into()],
                no_changes: true,
            },
        )
        .await
        .unwrap();
    let ctx = run_ctx(&tmp, &config, storage.clone(), Some("head-123"));
    let result = super::run::run_job(&ctx, demo_apply_job(&config)).await;
    assert!(
        matches!(
            result.outcome,
            Ok(Outcome::ApplyNoChanges { targeted: true, .. })
        ),
        "{:?}",
        result.outcome
    );
}

#[tokio::test]
async fn plan_without_changes_is_flagged_and_apply_skips_it() {
    if std::process::Command::new("terraform")
        .arg("version")
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        eprintln!("skipping: terraform not found on PATH");
        return;
    }
    let (tmp, config) = demo_fixture(true);
    // No resources at all: the plan is empty.
    std::fs::write(tmp.path().join("infra/demo/main.tf"), "").unwrap();
    let storage = Arc::new(MemoryStorage::default());
    let ctx = run_ctx(&tmp, &config, storage.clone(), Some("head-123"));
    let mut plan_job = demo_apply_job(&config);
    plan_job.command = Command::Plan;
    let result = super::run::run_job(&ctx, plan_job).await;
    match result.outcome {
        Ok(Outcome::Plan(p)) => assert!(p.is_empty()),
        other => panic!("expected plan outcome: {:?}", other),
    }
    assert!(
        storage
            .retrieve("o-r-1-demo.tfplan")
            .await
            .unwrap()
            .unwrap()
            .no_changes
    );

    let result = super::run::run_job(&ctx, demo_apply_job(&config)).await;
    assert!(
        matches!(
            result.outcome,
            Ok(Outcome::ApplyNoChanges { plan: None, .. })
        ),
        "{:?}",
        result.outcome
    );
    assert!(!storage.exists("o-r-1-demo.tfplan").await.unwrap());

    // Without plan storage the plan produced before the apply decides.
    let ctx = run_ctx(&tmp, &config, Arc::new(NoopPlanStorage), None);
    let result = super::run::run_job(&ctx, demo_apply_job(&config)).await;
    match result.outcome {
        Ok(Outcome::ApplyNoChanges { plan: Some(p), .. }) => assert!(p.is_empty()),
        other => panic!("expected skipped apply with its plan: {:?}", other),
    }
}

#[tokio::test]
async fn unhandled_events_are_a_noop() {
    use super::pr_event::build_trigger;
    use crate::github::{GithubClient, GithubContext};
    let tmp = tempfile::tempdir().unwrap();
    let config = load_from_str("projects:\n  - name: a\n    dir: a\n", tmp.path()).unwrap();
    // Never reached for these events; no server behind it.
    let github = GithubClient::new("tok", "o", "r", Some("http://127.0.0.1:9"));
    for event in [
        "push",
        "workflow_run",
        "check_suite",
        "workflow_dispatch",
        "schedule",
    ] {
        let ctx = GithubContext::parse(&format!(
            r#"{{"event_name":"{}","actor":"me","repository":"o/r","run_id":"1","run_attempt":"1","event":{{}}}}"#,
            event
        ))
        .unwrap();
        let result = build_trigger(&config, &ctx, &github).await;
        assert!(
            matches!(result, Ok(None)),
            "{} should be ignored, got {:?}",
            event,
            result.map(|t| t.is_some())
        );
    }
}

// ---------------------------------------------------------------------------
// Lock acquisition and the dangling-lock sweep.

use crate::lock::testing::MemLock;
use crate::lock::{Lock, PrLocker};
use crate::report::RowStatus;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn mock_pr_state(server: &MockServer, number: u64, state: &str) {
    Mock::given(method("GET"))
        .and(path(format!("/repos/o/r/pulls/{}", number)))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "head": {"ref": "x", "sha": "s"}, "base": {"ref": "main", "sha": "b"},
            "merged": false, "state": state, "draft": false
        })))
        .mount(server)
        .await;
}

fn pr_locker(server: &MockServer, backend: Arc<dyn Lock>) -> PrLocker {
    PrLocker::new(
        backend,
        crate::github::GithubClient::new("t", "o", "r", Some(&server.uri())),
    )
}

/// Jobs for projects a, b, c (configuration order) of PR 1 in `o/r`.
fn abc_jobs(command: Command) -> Vec<Job> {
    let tmp = tempfile::tempdir().unwrap();
    let c = load_from_str(
        "projects:\n  - name: a\n    dir: a\n  - name: b\n    dir: b\n  - name: c\n    dir: c\n",
        tmp.path(),
    )
    .unwrap();
    c.projects
        .iter()
        .map(|p| {
            Job::new(
                p,
                c.workflow_for(p),
                command,
                Some(1),
                "x",
                "me",
                "o/r",
                "",
                "",
            )
        })
        .collect()
}

fn names(jobs: &[Job]) -> Vec<&str> {
    jobs.iter().map(|j| j.project.name.as_str()).collect()
}

#[tokio::test]
async fn locks_all_free_are_all_taken() {
    let server = MockServer::start().await;
    let mem = Arc::new(MemLock::default());
    let locker = pr_locker(&server, mem.clone());
    let plan = acquire_locks(&locker, 1, abc_jobs(Command::Plan), PlanLocking::Take).await;
    assert!(!plan.failed);
    assert_eq!(names(&plan.runnable), vec!["a", "b", "c"]);
    assert!(plan.rows.is_empty());
    for r in ["o/r#a", "o/r#b", "o/r#c"] {
        assert_eq!(mem.holder(r).await.unwrap(), Some(1));
    }
}

#[tokio::test]
async fn locks_held_by_another_pr_block_new_acquisition() {
    let server = MockServer::start().await;
    mock_pr_state(&server, 2, "open").await;
    let mem = Arc::new(MemLock::holding(&[("o/r#b", 2)]));
    let locker = pr_locker(&server, mem.clone());
    let plan = acquire_locks(&locker, 1, abc_jobs(Command::Plan), PlanLocking::Take).await;
    assert!(plan.failed);
    assert!(plan.runnable.is_empty(), "{:?}", names(&plan.runnable));
    let rows: Vec<(&str, &RowStatus)> = plan.rows.iter().map(|(n, s, _)| (n.as_str(), s)).collect();
    assert_eq!(rows[0].0, "a");
    assert!(matches!(rows[0].1, RowStatus::Skipped(r) if r == "lock not taken: b is locked by #2"));
    assert_eq!(rows[1], ("b", &RowStatus::LockedByOther(2)));
    assert!(matches!(rows[2].1, RowStatus::Skipped(_)));
    // Nothing was taken for PR 1; PR 2 keeps its lock.
    assert_eq!(mem.holder("o/r#a").await.unwrap(), None);
    assert_eq!(mem.holder("o/r#b").await.unwrap(), Some(2));
    assert_eq!(mem.holder("o/r#c").await.unwrap(), None);
}

#[tokio::test]
async fn locks_already_ours_still_run_when_others_are_blocked() {
    let server = MockServer::start().await;
    mock_pr_state(&server, 2, "open").await;
    let mem = Arc::new(MemLock::holding(&[("o/r#a", 1), ("o/r#b", 2)]));
    let locker = pr_locker(&server, mem.clone());
    let mut jobs = abc_jobs(Command::Plan);
    jobs.push({
        let mut j = jobs[2].clone();
        j.command = Command::Unlock;
        j
    });
    let plan = acquire_locks(&locker, 1, jobs, PlanLocking::Take).await;
    assert!(plan.failed);
    // `a` is held from an earlier run, the unlock job needs no lock.
    assert_eq!(names(&plan.runnable), vec!["a", "c"]);
    assert_eq!(plan.runnable[1].command, Command::Unlock);
    assert_eq!(mem.holder("o/r#a").await.unwrap(), Some(1));
    assert_eq!(mem.holder("o/r#c").await.unwrap(), None);
}

/// Backend that looks free but refuses to hand out one resource, simulating
/// another PR taking it between the check and the acquisition.
struct RaceLock {
    inner: MemLock,
    race_on: String,
    other: u64,
    tripped: std::sync::atomic::AtomicBool,
}

#[async_trait::async_trait]
impl Lock for RaceLock {
    async fn lock(&self, pr: u64, resource: &str) -> anyhow::Result<bool> {
        if resource == self.race_on {
            self.tripped
                .store(true, std::sync::atomic::Ordering::SeqCst);
            return Ok(false);
        }
        self.inner.lock(pr, resource).await
    }
    async fn unlock(&self, resource: &str) -> anyhow::Result<bool> {
        self.inner.unlock(resource).await
    }
    async fn holder(&self, resource: &str) -> anyhow::Result<Option<u64>> {
        if resource == self.race_on && self.tripped.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok(Some(self.other));
        }
        self.inner.holder(resource).await
    }
    async fn held_by(&self, pr: u64, prefix: &str) -> anyhow::Result<Vec<String>> {
        self.inner.held_by(pr, prefix).await
    }
    fn name(&self) -> &'static str {
        "race"
    }
}

#[tokio::test]
async fn lost_race_releases_locks_taken_in_this_run() {
    let server = MockServer::start().await;
    mock_pr_state(&server, 2, "open").await;
    let race = Arc::new(RaceLock {
        inner: MemLock::default(),
        race_on: "o/r#b".into(),
        other: 2,
        tripped: Default::default(),
    });
    let locker = pr_locker(&server, race.clone());
    let plan = acquire_locks(&locker, 1, abc_jobs(Command::Plan), PlanLocking::Take).await;
    assert!(plan.failed);
    assert!(plan.runnable.is_empty(), "{:?}", names(&plan.runnable));
    let rows: Vec<(&str, &RowStatus)> = plan.rows.iter().map(|(n, s, _)| (n.as_str(), s)).collect();
    assert_eq!(rows[0], ("b", &RowStatus::LockedByOther(2)));
    assert!(
        matches!(rows[1], ("a", RowStatus::Skipped(r)) if r == "lock released: b is locked by #2")
    );
    assert!(
        matches!(rows[2], ("c", RowStatus::Skipped(r)) if r == "lock not taken: b is locked by #2")
    );
    // `a` was taken and given back; `c` was never attempted.
    assert_eq!(race.inner.holder("o/r#a").await.unwrap(), None);
    assert_eq!(race.inner.holder("o/r#c").await.unwrap(), None);
}

#[tokio::test]
async fn sweep_releases_only_this_prs_other_locks() {
    let server = MockServer::start().await;
    let mem = Arc::new(MemLock::holding(&[
        ("o/r#a", 1),
        ("o/r#eks", 1),
        ("o/r#z", 2),
        ("x/y#a", 1),
    ]));
    let locker = pr_locker(&server, mem.clone());
    let storage = MemoryStorage::with("o-r-1-eks.tfplan", Some("sha"));
    let released = sweep_locks(&locker, &storage, 1, "o/r", &["o/r#a".to_string()])
        .await
        .unwrap();
    assert_eq!(released, vec!["eks".to_string()]);
    assert_eq!(mem.holder("o/r#a").await.unwrap(), Some(1));
    assert_eq!(mem.holder("o/r#eks").await.unwrap(), None);
    assert_eq!(mem.holder("o/r#z").await.unwrap(), Some(2));
    assert_eq!(mem.holder("x/y#a").await.unwrap(), Some(1));
    assert!(!storage.exists("o-r-1-eks.tfplan").await.unwrap());
}

// ---------------------------------------------------------------------------
// Commit statuses when there is nothing to do.

#[test]
fn all_plans_empty_decisions() {
    assert!(all_plans_empty(&[plan_result(true), plan_result(true)]));
    assert!(!all_plans_empty(&[plan_result(true), plan_result(false)]));
    // A failure anywhere disqualifies.
    assert!(!all_plans_empty(&[
        plan_result(true),
        JobResult {
            project: "b".into(),
            command: Command::Plan,
            outcome: Err("boom".into()),
        }
    ]));
    // Lock / unlock jobs alongside empty plans are fine, but there must be
    // at least one plan.
    let unlocked = JobResult {
        project: "c".into(),
        command: Command::Unlock,
        outcome: Ok(Outcome::Unlocked(true)),
    };
    assert!(all_plans_empty(&[plan_result(true), unlocked]));
    assert!(!all_plans_empty(&[JobResult {
        project: "c".into(),
        command: Command::Unlock,
        outcome: Ok(Outcome::Unlocked(true)),
    }]));
    assert!(!all_plans_empty(&[]));
}

#[tokio::test]
async fn noop_statuses_set_plan_and_apply_to_success() {
    use wiremock::matchers::body_partial_json;
    let server = MockServer::start().await;
    for context in ["digger/plan", "digger/apply"] {
        Mock::given(method("POST"))
            .and(path("/repos/o/r/statuses/abc"))
            .and(body_partial_json(serde_json::json!({
                "state": "success",
                "context": context,
                "description": "no impacted projects",
                "target_url": "https://ci/run/1"
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;
    }
    let github = crate::github::GithubClient::new("t", "o", "r", Some(&server.uri()));
    set_noop_statuses(&github, "abc", "no impacted projects", "https://ci/run/1").await;
}

#[tokio::test]
async fn push_without_impacted_projects_expects_statuses() {
    use super::pr_event::build_trigger;
    use crate::github::{GithubClient, GithubContext};
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls/7/files"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"filename": "README.md"}
        ])))
        .mount(&server)
        .await;
    let tmp = tempfile::tempdir().unwrap();
    let config = load_from_str("projects:\n  - name: a\n    dir: infra/a\n", tmp.path()).unwrap();
    let github = GithubClient::new("tok", "o", "r", Some(&server.uri()));
    let ctx = |action: &str| {
        GithubContext::parse(&format!(
            r#"{{"event_name":"pull_request","actor":"me","repository":"o/r","run_id":"1","run_attempt":"1",
                "event":{{"action":"{}","pull_request":{{"number":7,"draft":false,"merged":false,
                "head":{{"ref":"feat","sha":"abc"}},"base":{{"ref":"main","sha":"def"}}}},
                "repository":{{"default_branch":"main","full_name":"o/r"}},"sender":{{"login":"me"}}}}}}"#,
            action
        ))
        .unwrap()
    };
    let t = build_trigger(&config, &ctx("synchronize"), &github)
        .await
        .unwrap()
        .expect("trigger");
    assert!(t.jobs.is_empty());
    assert_eq!(t.impacted, 0);
    assert_eq!(t.head_sha, "abc");
    assert!(t.expects_plan_or_apply);
    // Closing a PR is not a plan / apply event.
    let t = build_trigger(&config, &ctx("closed"), &github)
        .await
        .unwrap()
        .expect("trigger");
    assert!(!t.expects_plan_or_apply);
    assert!(t.release_all_locks);
}

#[test]
fn comment_commands_on_closed_prs() {
    let open = pr(false, "open", Some(true), Some("clean"));
    let closed = pr(false, "closed", None, None);
    let merged = pr(true, "closed", None, None);
    for c in [
        Command::Plan,
        Command::Apply,
        Command::Lock,
        Command::Unlock,
    ] {
        assert!(closed_pr_rejection(&open, c).is_none(), "{:?}", c);
    }
    let r = closed_pr_rejection(&closed, Command::Plan).unwrap();
    assert!(r.contains("is closed"), "{}", r);
    assert!(r.contains("`plan`"), "{}", r);
    assert!(r.contains("`nikama unlock`"), "{}", r);
    let r = closed_pr_rejection(&merged, Command::Apply).unwrap();
    assert!(r.contains("is merged"), "{}", r);
    assert!(closed_pr_rejection(&merged, Command::Lock).is_some());
    // Unlock is always accepted.
    assert!(closed_pr_rejection(&closed, Command::Unlock).is_none());
    assert!(closed_pr_rejection(&merged, Command::Unlock).is_none());
}

#[test]
fn comment_commands_on_draft_prs() {
    let mut draft = pr(false, "open", None, None);
    draft.draft = true;
    let open = pr(false, "open", Some(true), Some("clean"));
    for c in [Command::Plan, Command::Apply, Command::Lock] {
        let r = draft_pr_rejection(&draft, c, false).unwrap();
        assert!(r.contains("is a draft"), "{}", r);
        assert!(r.contains(&format!("`{}`", c)), "{}", r);
        assert!(r.contains("`nikama unlock`"), "{}", r);
        // allow_draft_prs lifts the restriction; non-drafts are never affected.
        assert!(draft_pr_rejection(&draft, c, true).is_none(), "{:?}", c);
        assert!(draft_pr_rejection(&open, c, false).is_none(), "{:?}", c);
    }
    // Unlock is always accepted.
    assert!(draft_pr_rejection(&draft, Command::Unlock, false).is_none());
}

#[tokio::test]
async fn plan_comment_on_merged_pr_is_rejected_with_a_comment() {
    use super::pr_event::build_trigger;
    use crate::github::{GithubClient, GithubContext};
    use wiremock::matchers::body_string_contains;
    let server = MockServer::start().await;
    mock_role(&server, "me", "write").await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls/7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "head": {"ref": "feat", "sha": "abc"}, "base": {"ref": "main", "sha": "def"},
            "user": {"login": "alice"},
            "merged": true, "state": "closed", "draft": false
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/o/r/issues/7/comments"))
        .and(body_string_contains("this pull request is merged"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 1})))
        .expect(1)
        .mount(&server)
        .await;
    let tmp = tempfile::tempdir().unwrap();
    let config = load_from_str("projects:\n  - name: a\n    dir: a\n", tmp.path()).unwrap();
    let github = GithubClient::new("tok", "o", "r", Some(&server.uri()));
    let ctx = GithubContext::parse(
        r#"{"event_name":"issue_comment","actor":"me","repository":"o/r","run_id":"1","run_attempt":"1",
            "event":{"action":"created","issue":{"number":7,"pull_request":{}},
            "comment":{"id":99,"body":"digger plan"},
            "repository":{"default_branch":"main","full_name":"o/r"},"sender":{"login":"me"}}}"#,
    )
    .unwrap();
    let result = build_trigger(&config, &ctx, &github).await;
    assert!(
        matches!(result, Ok(None)),
        "{:?}",
        result.map(|t| t.is_some())
    );
    // The changed-files endpoint was never called: rejection happens first.
    assert!(server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .all(|r| !r.url.path().ends_with("/files")));
}

// ---------------------------------------------------------------------------
// Comment commands are only run for users with write access.

#[test]
fn write_access_roles() {
    use super::pr_event::has_write_access;
    for role in ["admin", "maintain", "write", "Write"] {
        assert!(has_write_access(role), "{}", role);
    }
    for role in ["triage", "read", "none", ""] {
        assert!(!has_write_access(role), "{}", role);
    }
}

fn comment_context(login: &str, body: &str) -> crate::github::GithubContext {
    crate::github::GithubContext::parse(&format!(
        r#"{{"event_name":"issue_comment","actor":"{login}","repository":"o/r","run_id":"1","run_attempt":"1",
            "event":{{"action":"created",
                "issue":{{"number":5,"pull_request":{{"url":"u"}}}},
                "comment":{{"id":9,"body":"{body}","user":{{"login":"{login}"}},"author_association":"COLLABORATOR"}},
                "repository":{{"default_branch":"main","full_name":"o/r"}},
                "sender":{{"login":"{login}"}}}}}}"#,
    ))
    .unwrap()
}

async fn mock_role(server: &MockServer, login: &str, role: &str) {
    Mock::given(method("GET"))
        .and(path(format!(
            "/repos/o/r/collaborators/{}/permission",
            login
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "permission": "read", "role_name": role
        })))
        .mount(server)
        .await;
}

#[tokio::test]
async fn comment_command_from_read_only_user_is_refused() {
    use super::pr_event::build_trigger;
    use crate::github::GithubClient;
    let tmp = tempfile::tempdir().unwrap();
    let config = load_from_str("projects:\n  - name: a\n    dir: a\n", tmp.path()).unwrap();
    let server = MockServer::start().await;
    mock_role(&server, "eve", "read").await;
    Mock::given(method("POST"))
        .and(path("/repos/o/r/issues/5/comments"))
        .and(wiremock::matchers::body_partial_json(serde_json::json!({
            "body": "**nikama**: `plan` was not run: eve does not have write access to this repository."
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 1})))
        .expect(1)
        .mount(&server)
        .await;
    let github = GithubClient::new("tok", "o", "r", Some(&server.uri()));

    let result = build_trigger(&config, &comment_context("eve", "digger plan"), &github).await;
    assert!(
        matches!(result, Ok(None)),
        "{:?}",
        result.map(|t| t.is_some())
    );
    // Nothing beyond the permission check and the reply was requested.
    let paths: Vec<String> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| r.url.path().to_string())
        .collect();
    assert_eq!(
        paths,
        vec![
            "/repos/o/r/collaborators/eve/permission",
            "/repos/o/r/issues/5/comments"
        ]
    );
}

#[tokio::test]
async fn comment_command_from_writer_proceeds() {
    use super::pr_event::build_trigger;
    use crate::github::GithubClient;
    let tmp = tempfile::tempdir().unwrap();
    let config = load_from_str("projects:\n  - name: a\n    dir: a\n", tmp.path()).unwrap();
    let server = MockServer::start().await;
    mock_role(&server, "alice", "write").await;
    mock_pr_state(&server, 5, "open").await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls/5/files"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!([{"filename": "a/main.tf"}])),
        )
        .mount(&server)
        .await;
    let github = GithubClient::new("tok", "o", "r", Some(&server.uri()));

    let trigger = build_trigger(&config, &comment_context("alice", "digger plan"), &github)
        .await
        .unwrap()
        .expect("trigger");
    assert_eq!(trigger.pr_number, 5);
    assert_eq!(trigger.jobs.len(), 1);
    assert_eq!(trigger.jobs[0].command, Command::Plan);
    assert_eq!(trigger.jobs[0].requested_by, "alice");
}

#[tokio::test]
async fn plan_comment_on_draft_pr_is_rejected_with_a_comment() {
    use super::pr_event::build_trigger;
    use crate::github::GithubClient;
    use wiremock::matchers::body_string_contains;
    let server = MockServer::start().await;
    mock_role(&server, "alice", "write").await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls/5"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "head": {"ref": "feat", "sha": "abc"}, "base": {"ref": "main", "sha": "def"},
            "user": {"login": "alice"},
            "merged": false, "state": "open", "draft": true
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/o/r/issues/5/comments"))
        .and(body_string_contains("this pull request is a draft"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 1})))
        .expect(1)
        .mount(&server)
        .await;
    let tmp = tempfile::tempdir().unwrap();
    let config = load_from_str("projects:\n  - name: a\n    dir: a\n", tmp.path()).unwrap();
    let github = GithubClient::new("tok", "o", "r", Some(&server.uri()));

    let result = build_trigger(&config, &comment_context("alice", "digger plan"), &github).await;
    assert!(
        matches!(result, Ok(None)),
        "{:?}",
        result.map(|t| t.is_some())
    );
    // The changed-files endpoint was never called: rejection happens first.
    assert!(server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .all(|r| !r.url.path().ends_with("/files")));
}

// ---------------------------------------------------------------------------
// Planning without locks (`plan --no-lock`, `no_lock_label`).

#[tokio::test]
async fn no_lock_plan_takes_nothing_and_runs_despite_other_holders() {
    let server = MockServer::start().await;
    mock_pr_state(&server, 2, "open").await;
    // a: held by this PR from an earlier run, b: held by PR 2, c: free.
    let mem = Arc::new(MemLock::holding(&[("o/r#a", 1), ("o/r#b", 2)]));
    let locker = pr_locker(&server, mem.clone());
    let plan = acquire_locks(
        &locker,
        1,
        abc_jobs(Command::Plan),
        PlanLocking::SkipAndRelease,
    )
    .await;
    assert!(!plan.failed);
    assert_eq!(names(&plan.runnable), vec!["a", "b", "c"]);
    assert!(plan.rows.is_empty());
    assert_eq!(plan.released, vec!["a"]);
    assert_eq!(mem.holder("o/r#a").await.unwrap(), None);
    assert_eq!(mem.holder("o/r#b").await.unwrap(), Some(2));
    assert_eq!(mem.holder("o/r#c").await.unwrap(), None);
}

#[tokio::test]
async fn locks_taken_in_this_run_are_recorded() {
    let server = MockServer::start().await;
    // a: held by this PR from an earlier run, b and c: free.
    let mem = Arc::new(MemLock::holding(&[("o/r#a", 1)]));
    let locker = pr_locker(&server, mem.clone());
    let plan = acquire_locks(&locker, 1, abc_jobs(Command::Plan), PlanLocking::Take).await;
    assert!(!plan.failed);
    assert_eq!(
        plan.taken,
        vec![
            ("b".to_string(), "o/r#b".to_string()),
            ("c".to_string(), "o/r#c".to_string())
        ]
    );
}

#[tokio::test]
async fn no_change_plans_release_only_locks_taken_in_this_run() {
    let server = MockServer::start().await;
    // a: held from an earlier run (maybe an apply), b and c: taken now.
    let mem = Arc::new(MemLock::holding(&[
        ("o/r#a", 1),
        ("o/r#b", 1),
        ("o/r#c", 1),
    ]));
    let locker = pr_locker(&server, mem.clone());
    let taken = vec![
        ("b".to_string(), "o/r#b".to_string()),
        ("c".to_string(), "o/r#c".to_string()),
    ];
    let result = |project: &str, empty: bool| {
        let mut r = plan_result(empty);
        r.project = project.into();
        r
    };
    // a and b: no changes; c: changes.
    let results = vec![result("a", true), result("b", true), result("c", false)];
    let released = release_no_change_locks(&locker, 1, &results, &taken).await;
    assert_eq!(released, vec!["b".to_string()]);
    assert_eq!(mem.holder("o/r#a").await.unwrap(), Some(1));
    assert_eq!(mem.holder("o/r#b").await.unwrap(), None);
    assert_eq!(mem.holder("o/r#c").await.unwrap(), Some(1));
}

#[tokio::test]
async fn failed_plans_keep_their_lock() {
    let server = MockServer::start().await;
    let mem = Arc::new(MemLock::holding(&[("o/r#a", 1)]));
    let locker = pr_locker(&server, mem.clone());
    let taken = vec![("a".to_string(), "o/r#a".to_string())];
    let results = vec![JobResult {
        project: "a".into(),
        command: Command::Plan,
        outcome: Err("boom".into()),
    }];
    let released = release_no_change_locks(&locker, 1, &results, &taken).await;
    assert!(released.is_empty());
    assert_eq!(mem.holder("o/r#a").await.unwrap(), Some(1));
}

#[tokio::test]
async fn no_lock_does_not_apply_to_apply_jobs() {
    let server = MockServer::start().await;
    mock_pr_state(&server, 2, "open").await;
    let mem = Arc::new(MemLock::holding(&[("o/r#b", 2)]));
    let locker = pr_locker(&server, mem.clone());
    let plan = acquire_locks(
        &locker,
        1,
        abc_jobs(Command::Apply),
        PlanLocking::SkipAndRelease,
    )
    .await;
    assert!(plan.failed);
    assert!(plan.runnable.is_empty(), "{:?}", names(&plan.runnable));
    assert_eq!(mem.holder("o/r#b").await.unwrap(), Some(2));
}

fn trigger_with(jobs: Vec<Job>, no_lock_flag: bool) -> super::pr_event::Trigger {
    super::pr_event::Trigger {
        pr_number: 1,
        jobs,
        covers_all_impacted: true,
        notes: vec![],
        comment_id: None,
        release_all_locks: false,
        head_sha: "s".into(),
        impacted: 3,
        expects_plan_or_apply: true,
        no_lock_flag,
        extra_args: vec![],
    }
}

#[test]
fn no_lock_reason_from_flag_or_label() {
    let tmp = tempfile::tempdir().unwrap();
    let plain = load_from_str("projects:\n  - name: a\n    dir: a\n", tmp.path()).unwrap();
    let labelled = load_from_str(
        "projects:\n  - name: a\n    dir: a\nno_lock_label: No-Lock\n",
        tmp.path(),
    )
    .unwrap();
    let mut with_label = pr(false, "open", Some(true), Some("clean"));
    with_label.labels = vec!["bug".into(), "no-lock".into()];
    let without_label = pr(false, "open", Some(true), Some("clean"));

    // The comment flag works without any configuration.
    assert_eq!(
        no_lock_reason(
            &trigger_with(abc_jobs(Command::Plan), true),
            &plain,
            &without_label
        ),
        Some("`--no-lock`".into())
    );
    assert_eq!(
        no_lock_reason(
            &trigger_with(abc_jobs(Command::Plan), false),
            &plain,
            &with_label
        ),
        None
    );
    // The label is matched case-insensitively, only when configured.
    assert_eq!(
        no_lock_reason(
            &trigger_with(abc_jobs(Command::Plan), false),
            &labelled,
            &with_label
        ),
        Some("label `No-Lock`".into())
    );
    assert_eq!(
        no_lock_reason(
            &trigger_with(abc_jobs(Command::Plan), false),
            &labelled,
            &without_label
        ),
        None
    );
    // Runs that plan nothing never qualify.
    assert_eq!(
        no_lock_reason(
            &trigger_with(abc_jobs(Command::Apply), true),
            &labelled,
            &with_label
        ),
        None
    );
}

#[tokio::test]
async fn plan_comment_with_no_lock_sets_the_flag() {
    use super::pr_event::build_trigger;
    use crate::github::GithubClient;
    let tmp = tempfile::tempdir().unwrap();
    let config = load_from_str("projects:\n  - name: a\n    dir: a\n", tmp.path()).unwrap();
    let server = MockServer::start().await;
    mock_role(&server, "alice", "write").await;
    mock_pr_state(&server, 5, "open").await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls/5/files"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!([{"filename": "a/main.tf"}])),
        )
        .mount(&server)
        .await;
    let github = GithubClient::new("tok", "o", "r", Some(&server.uri()));

    let trigger = build_trigger(
        &config,
        &comment_context("alice", "digger plan --no-lock"),
        &github,
    )
    .await
    .unwrap()
    .expect("trigger");
    assert!(trigger.no_lock_flag);
    assert_eq!(trigger.jobs.len(), 1);
    let trigger = build_trigger(&config, &comment_context("alice", "digger plan"), &github)
        .await
        .unwrap()
        .expect("trigger");
    assert!(!trigger.no_lock_flag);
}

#[tokio::test]
async fn push_skips_projects_whose_branch_does_not_match_the_base() {
    use super::pr_event::build_trigger;
    use crate::github::{GithubClient, GithubContext};
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls/7/files"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"filename": "infra/app/main.tf"}
        ])))
        .mount(&server)
        .await;
    let tmp = tempfile::tempdir().unwrap();
    let config = load_from_str(
        "projects:\n  - name: staging\n    dir: infra/app\n    workspace: staging\n    branch: ^develop$\n  - name: prod\n    dir: infra/app\n    workspace: prod\n    branch: ^main$\n",
        tmp.path(),
    )
    .unwrap();
    let github = GithubClient::new("tok", "o", "r", Some(&server.uri()));
    let ctx = |base: &str| {
        GithubContext::parse(&format!(
            r#"{{"event_name":"pull_request","actor":"me","repository":"o/r","run_id":"1","run_attempt":"1",
                "event":{{"action":"synchronize","pull_request":{{"number":7,"draft":false,"merged":false,
                "head":{{"ref":"feat","sha":"abc"}},"base":{{"ref":"{}","sha":"def"}}}},
                "repository":{{"default_branch":"main","full_name":"o/r"}},"sender":{{"login":"me"}}}}}}"#,
            base
        ))
        .unwrap()
    };
    let t = build_trigger(&config, &ctx("develop"), &github)
        .await
        .unwrap()
        .expect("trigger");
    assert_eq!(t.impacted, 1);
    assert_eq!(t.jobs[0].project.name, "staging");
    let t = build_trigger(&config, &ctx("main"), &github)
        .await
        .unwrap()
        .expect("trigger");
    assert_eq!(t.jobs[0].project.name, "prod");
    let t = build_trigger(&config, &ctx("feature/x"), &github)
        .await
        .unwrap()
        .expect("trigger");
    assert!(t.jobs.is_empty());
}

#[test]
fn branch_filter_notes_explain_unmatched_selectors() {
    use super::pr_event::branch_filter_notes;
    let tmp = tempfile::tempdir().unwrap();
    let config = load_from_str(
        "projects:\n  - name: prod\n    dir: infra/app\n    alias: production\n    branch: ^main$\n  - name: any\n    dir: infra/any\n",
        tmp.path(),
    )
    .unwrap();
    let notes = branch_filter_notes(
        &config,
        &[
            "prod".into(),
            "production".into(),
            "any".into(),
            "nope".into(),
        ],
        "develop",
    );
    assert_eq!(notes.len(), 2);
    assert!(notes[0].contains(
        "project 'prod' only runs for pull requests into branches matching `^main$`, not `develop`"
    ));
    assert!(notes[1].starts_with("project 'production'"));
    assert!(branch_filter_notes(&config, &["prod".into()], "main").is_empty());
}

// `pr_locks: on_apply`: plans never lock and leave existing locks alone.

#[tokio::test]
async fn on_apply_plan_takes_nothing_and_keeps_our_lock() {
    let server = MockServer::start().await;
    mock_pr_state(&server, 2, "open").await;
    // a: held by this PR from an earlier apply, b: held by PR 2, c: free.
    let mem = Arc::new(MemLock::holding(&[("o/r#a", 1), ("o/r#b", 2)]));
    let locker = pr_locker(&server, mem.clone());
    let plan = acquire_locks(&locker, 1, abc_jobs(Command::Plan), PlanLocking::Skip).await;
    assert!(!plan.failed);
    assert_eq!(names(&plan.runnable), vec!["a", "b", "c"]);
    assert!(plan.rows.is_empty());
    assert!(plan.released.is_empty());
    assert_eq!(mem.holder("o/r#a").await.unwrap(), Some(1));
    assert_eq!(mem.holder("o/r#b").await.unwrap(), Some(2));
    assert_eq!(mem.holder("o/r#c").await.unwrap(), None);
}

#[test]
fn no_lock_escapes_are_moot_with_on_apply() {
    let tmp = tempfile::tempdir().unwrap();
    let config = load_from_str(
        "projects:\n  - name: a\n    dir: a\npr_locks: on_apply\nno_lock_label: no-lock\n",
        tmp.path(),
    )
    .unwrap();
    let mut pr_info = pr(false, "open", Some(true), Some("clean"));
    pr_info.labels = vec!["no-lock".into()];
    assert_eq!(
        no_lock_reason(
            &trigger_with(abc_jobs(Command::Plan), true),
            &config,
            &pr_info
        ),
        None
    );
}

#[tokio::test]
async fn on_apply_apply_still_locks() {
    let server = MockServer::start().await;
    mock_pr_state(&server, 2, "open").await;
    let mem = Arc::new(MemLock::holding(&[]));
    let locker = pr_locker(&server, mem.clone());
    let plan = acquire_locks(&locker, 1, abc_jobs(Command::Apply), PlanLocking::Skip).await;
    assert!(!plan.failed);
    assert_eq!(names(&plan.runnable), vec!["a", "b", "c"]);
    assert_eq!(mem.holder("o/r#a").await.unwrap(), Some(1));
    assert_eq!(mem.holder("o/r#c").await.unwrap(), Some(1));
}

#[test]
fn extra_args_rejection_cases() {
    use super::pr_event::extra_args_rejection;
    let tmp = tempfile::tempdir().unwrap();
    let off = load_from_str("projects: []\n", tmp.path()).unwrap();
    let on = load_from_str(
        "projects: []\nallowed_plan_args: ['-target=*', '-refresh=false']\n",
        tmp.path(),
    )
    .unwrap();
    let target = vec!["-target=module.a".to_string()];
    assert_eq!(extra_args_rejection(&off, Command::Plan, &[], false), None);
    assert!(extra_args_rejection(&off, Command::Plan, &target, false)
        .unwrap()
        .contains("not enabled in this repository"));
    assert_eq!(
        extra_args_rejection(&on, Command::Plan, &target, false),
        None
    );
    assert_eq!(
        extra_args_rejection(&on, Command::Plan, &target, true),
        None
    );
    // Apply without plan storage plans right before applying, with the args.
    assert_eq!(
        extra_args_rejection(&on, Command::Apply, &target, false),
        None
    );
    let msg = extra_args_rejection(&on, Command::Apply, &target, true).unwrap();
    assert!(msg.contains("applies the stored plan as it is"), "{msg}");
    assert!(msg.contains("`nikama plan`"), "{msg}");
    let msg = extra_args_rejection(
        &on,
        Command::Plan,
        &["-target=x".into(), "-lock=false".into()],
        false,
    )
    .unwrap();
    assert!(msg.contains("argument `-lock=false` is not allowed by `allowed_plan_args` (`-target=*`, `-refresh=false`)"), "{msg}");
}

#[tokio::test]
async fn plan_comment_with_extra_args_sets_them_on_every_job() {
    use super::pr_event::build_trigger;
    use crate::github::GithubClient;
    let tmp = tempfile::tempdir().unwrap();
    let config = load_from_str(
        "projects:\n  - name: a\n    dir: a\n  - name: b\n    dir: b\nallowed_plan_args: ['-target=*']\n",
        tmp.path(),
    )
    .unwrap();
    let server = MockServer::start().await;
    mock_role(&server, "alice", "write").await;
    mock_pr_state(&server, 5, "open").await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls/5/files"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!([{"filename": "a/main.tf"}, {"filename": "b/main.tf"}]),
        ))
        .mount(&server)
        .await;
    let posted = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let p2 = posted.clone();
    Mock::given(method("POST"))
        .and(path("/repos/o/r/issues/5/comments"))
        .respond_with(move |req: &wiremock::Request| {
            let body: serde_json::Value = req.body_json().unwrap();
            p2.lock()
                .unwrap()
                .push(body["body"].as_str().unwrap().to_string());
            ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 1}))
        })
        .mount(&server)
        .await;
    let github = GithubClient::new("tok", "o", "r", Some(&server.uri()));

    let trigger = build_trigger(
        &config,
        &comment_context("alice", "digger plan -p a -- -target=module.x"),
        &github,
    )
    .await
    .unwrap()
    .expect("trigger");
    assert_eq!(trigger.extra_args, vec!["-target=module.x"]);
    assert_eq!(trigger.jobs.len(), 1);
    assert_eq!(trigger.jobs[0].extra_args, vec!["-target=module.x"]);
    assert!(trigger
        .notes
        .iter()
        .any(|n| n.contains("`plan` runs with extra arguments `-target=module.x`")));

    // A disallowed argument is refused with a comment and nothing runs.
    let err = match build_trigger(
        &config,
        &comment_context("alice", "digger plan -- -lock=false"),
        &github,
    )
    .await
    {
        Err(code) => code,
        Ok(_) => panic!("expected a refusal"),
    };
    assert_eq!(err, crate::modes::exit::COMMAND);
    let bodies = posted.lock().unwrap().clone();
    assert!(
        bodies
            .iter()
            .any(|b| b.contains("`-lock=false` is not allowed")),
        "{bodies:?}"
    );

    let trigger = build_trigger(&config, &comment_context("alice", "digger plan"), &github)
        .await
        .unwrap()
        .expect("trigger");
    assert!(trigger.extra_args.is_empty());
    assert!(trigger.jobs.iter().all(|j| j.extra_args.is_empty()));
}
