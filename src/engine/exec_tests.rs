//! Engine tests. Tests that need a real `terraform` binary skip themselves
//! when it is not on PATH.

use super::runner::{apply_args, destroy_args, init_args, plan_args};
use super::*;
use crate::config::{load_from_str, Config};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn terraform_available() -> bool {
    std::process::Command::new("terraform")
        .arg("version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

macro_rules! require_terraform {
    () => {
        if !terraform_available() {
            eprintln!("skipping: terraform not found on PATH");
            return;
        }
    };
}

#[test]
fn runner_argument_builders() {
    assert_eq!(
        init_args(&["-upgrade".into(), "".into()]),
        vec!["init", "-upgrade", "-input=false", "-no-color"]
    );
    assert_eq!(
        plan_args(
            &["-var-file=a.tfvars".into()],
            &["-target=module.x".into(), "-var=p=$HOME".into()],
            Path::new("/tmp/p.tfplan")
        ),
        // Request arguments follow the workflow's and are not expanded.
        vec![
            "plan",
            "-var-file=a.tfvars",
            "-target=module.x",
            "-var=p=$HOME",
            "-input=false",
            "-no-color",
            "-detailed-exitcode",
            "-lock-timeout=3m",
            "-out",
            "/tmp/p.tfplan"
        ]
    );
    assert_eq!(
        apply_args(&[], Some(Path::new("p.tfplan"))),
        vec![
            "apply",
            "-lock-timeout=3m",
            "-input=false",
            "-no-color",
            "-auto-approve",
            "p.tfplan"
        ]
    );
    assert_eq!(
        apply_args(&["-parallelism=2".into()], None),
        vec![
            "apply",
            "-parallelism=2",
            "-lock-timeout=3m",
            "-input=false",
            "-no-color",
            "-auto-approve"
        ]
    );
    assert_eq!(
        destroy_args(&[]),
        vec![
            "destroy",
            "-lock-timeout=3m",
            "-input=false",
            "-no-color",
            "-auto-approve"
        ]
    );
}

#[test]
fn runner_args_expand_env() {
    let _g = crate::util::test_env_guard();
    std::env::set_var("NIKAMA_TEST_VARFILE", "env.tfvars");
    assert_eq!(
        init_args(&["-backend-config=$NIKAMA_TEST_VARFILE".into()])[1],
        "-backend-config=env.tfvars"
    );
    std::env::remove_var("NIKAMA_TEST_VARFILE");
}

#[test]
fn job_names_and_tool() {
    let c = load_from_str(
        "projects:\n  - name: a\n    dir: a\n    opentofu: true\n",
        tempfile::tempdir().unwrap().path(),
    )
    .unwrap();
    let p = &c.projects[0];
    let job = Job::new(
        p,
        c.workflow_for(p),
        Command::Plan,
        Some(12),
        "pull_request",
        "me",
        "own/repo",
        "feat",
        "main",
    );
    assert_eq!(job.lock_id(), "own/repo#a");
    assert_eq!(job.plan_file_name(), "own-repo-12-a.tfplan");
    assert_eq!(job.tool(), IacTool::OpenTofu);
    assert_eq!(job.run_env["PR_BRANCH"], "feat");
    assert_eq!(job.run_env["DEFAULT_BRANCH"], "main");
    assert_eq!(job.run_env["PROJECT_DIR"], "a");
    assert!(!job.skip_merge_check);
    let manual = Job::new(
        p,
        c.workflow_for(p),
        Command::Plan,
        None,
        "manual",
        "me",
        "own/repo",
        "",
        "",
    );
    assert_eq!(manual.plan_file_name(), "own-repo-a.tfplan");
}

#[test]
fn apply_requirements_do_not_imply_skip_merge_check() {
    let c = load_from_str(
        "projects:\n  - name: a\n    dir: a\n    apply_requirements: [approved]\n",
        tempfile::tempdir().unwrap().path(),
    )
    .unwrap();
    let p = &c.projects[0];
    let job = Job::new(
        p,
        c.workflow_for(p),
        Command::Apply,
        Some(1),
        "x",
        "me",
        "o/r",
        "",
        "",
    );
    assert!(!job.skip_merge_check);
    assert_eq!(job.project.apply_requirements, vec!["approved".to_string()]);
}

#[test]
fn run_steps_get_command_env_and_nikama_variables() {
    let f = fixture(
        DEMO_TF,
        "    env_vars:\n      commands:\n        - name: TF_VAR_x\n          value: from-config\n        - name: PROJECT_NAME\n          value: evil\n        - name: TF_DATA_DIR\n          value: evil\n    plan:\n      steps: [init, plan]\n",
    );
    let j = job(&f, Command::Plan);
    let exec = executor(&f, &j);
    let base = BTreeMap::from([("TF_DATA_DIR".to_string(), "/tmp/data".to_string())]);
    let env = exec.run_env(Some(Path::new("/tmp/plan.tfplan")), &base);
    // `env_vars.commands` reach run steps ...
    assert_eq!(env["TF_VAR_x"], "from-config");
    // ... but cannot override what nikama sets itself.
    assert_eq!(env["PROJECT_NAME"], "demo");
    assert_eq!(env["PROJECT_DIR"], "infra/demo");
    assert_eq!(env["PR_BRANCH"], "feat");
    assert_eq!(env["DEFAULT_BRANCH"], "main");
    assert_eq!(env["TF_IN_AUTOMATION"], "true");
    assert_eq!(env["TF_DATA_DIR"], "/tmp/data");
    assert_eq!(env["NIKAMA_PLANFILE"], "/tmp/plan.tfplan");
    assert_eq!(env["DIGGER_PLANFILE"], "/tmp/plan.tfplan");
    assert!(env["NIKAMA_OUT"].ends_with("additional-output.log"));
    assert_eq!(env["NIKAMA_OUT"], env["DIGGER_OUT"]);
    // Before a plan step there is no plan file variable.
    assert!(!exec.run_env(None, &base).contains_key("NIKAMA_PLANFILE"));
}

struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    config: Config,
}

fn fixture(main_tf: &str, workflow_yaml: &str) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    std::fs::create_dir_all(root.join("infra/demo")).unwrap();
    std::fs::write(root.join("infra/demo/main.tf"), main_tf).unwrap();
    let yaml = format!(
        "projects:\n  - name: demo\n    dir: infra/demo\nworkflows:\n  default:\n{}",
        workflow_yaml
    );
    let config = load_from_str(&yaml, &root).unwrap();
    Fixture {
        _tmp: tmp,
        root,
        config,
    }
}

fn job(f: &Fixture, command: Command) -> Job {
    let p = &f.config.projects[0];
    Job::new(
        p,
        f.config.workflow_for(p),
        command,
        Some(1),
        "test",
        "me",
        "o/r",
        "feat",
        "main",
    )
}

fn executor<'a>(f: &Fixture, job: &'a Job) -> JobExecutor<'a> {
    JobExecutor {
        job,
        repo_root: f.root.clone(),
        work_dir: f.root.join("work"),
        init_lock: None,
        isolate_data_dir: false,
        cli_config: None,
    }
}

const DEMO_TF: &str = r#"
terraform {
  required_version = ">= 1.4"
}
resource "terraform_data" "demo" {
  input = "hello-world"
}
output "value" { value = terraform_data.demo.output }
"#;

#[tokio::test]
async fn plan_apply_destroy_roundtrip() {
    require_terraform!();
    let f = fixture(
        DEMO_TF,
        "    plan:\n      filter_regex: \"hello-\\\\w+\"\n      steps:\n        - init\n        - plan\n        - run: \"echo from-run-step >> $NIKAMA_OUT\"\n",
    );

    let plan_job = job(&f, Command::Plan);
    let exec = executor(&f, &plan_job);
    let plan = exec.plan().await.expect("plan succeeds");
    assert_eq!(plan.summary.create, 1);
    assert!(!plan.is_empty());
    assert!(plan.plan_file.exists());
    assert!(plan
        .plan_text
        .contains("terraform_data.demo will be created"));
    assert!(
        plan.plan_text.contains("<REDACTED>"),
        "filter_regex applied: {}",
        plan.plan_text
    );
    assert!(!plan.plan_text.contains("hello-world"));
    assert_eq!(plan.additional_output.as_deref(), Some("from-run-step"));
    assert_eq!(
        plan.steps
            .iter()
            .map(|s| s.step.as_str())
            .collect::<Vec<_>>(),
        vec!["init", "plan", "run"]
    );
    assert!(plan.plan_json.contains("resource_changes"));

    let apply_job = job(&f, Command::Apply);
    let exec = executor(&f, &apply_job);
    let apply = exec
        .apply(Some(&plan.plan_file))
        .await
        .expect("apply succeeds");
    assert_eq!(
        apply.summary,
        Some(ApplySummary {
            added: 1,
            ..Default::default()
        })
    );
    assert!(apply.output.contains("Apply complete!"));

    let exec = executor(&f, &plan_job);
    let second = exec.plan().await.expect("second plan succeeds");
    assert!(
        second.is_empty(),
        "plan after apply should be empty: {}",
        second.plan_text
    );
    assert!(second.plan_text.starts_with("No changes."));

    let destroy_job = job(&f, Command::Destroy);
    let exec = executor(&f, &destroy_job);
    let destroyed = exec.destroy().await.expect("destroy succeeds");
    assert_eq!(
        destroyed.summary,
        Some(ApplySummary {
            destroyed: 1,
            ..Default::default()
        })
    );
}

#[tokio::test]
async fn plan_failure_reports_the_failing_step() {
    require_terraform!();
    let f = fixture(
        "resource \"terraform_data\" \"x\" {\n  input = \n}\n",
        "    plan:\n      steps: [init, plan]\n",
    );
    let j = job(&f, Command::Plan);
    let err = executor(&f, &j)
        .plan()
        .await
        .expect_err("broken HCL must fail");
    assert_eq!(err.step, "init");
    assert!(!err.stderr.is_empty() || !err.stdout.is_empty());
}

#[tokio::test]
async fn run_step_failure_is_reported() {
    require_terraform!();
    let f = fixture(
        DEMO_TF,
        "    plan:\n      steps:\n        - init\n        - run: \"exit 3\"\n        - plan\n",
    );
    let j = job(&f, Command::Plan);
    let err = executor(&f, &j)
        .plan()
        .await
        .expect_err("run step must fail");
    assert_eq!(err.step, "run");
    assert!(err.message.contains("code 3"));
}

#[tokio::test]
async fn missing_plan_step_without_plan_file_errors() {
    require_terraform!();
    let f = fixture(
        DEMO_TF,
        "    plan:\n      steps:\n        - init\n        - run: \"true\"\n",
    );
    let j = job(&f, Command::Plan);
    let err = executor(&f, &j).plan().await.expect_err("no plan file");
    assert_eq!(err.step, "plan");
    assert!(err.message.contains("no plan file was produced"));
}

#[tokio::test]
async fn nonexistent_project_dir_fails_fast() {
    // Works with or without terraform: the working directory does not exist.
    let tmp = tempfile::tempdir().unwrap();
    let config = load_from_str(
        "projects:\n  - name: ghost\n    dir: does/not/exist\n",
        tmp.path(),
    )
    .unwrap();
    let p = &config.projects[0];
    let j = Job::new(
        p,
        config.workflow_for(p),
        Command::Plan,
        None,
        "t",
        "me",
        "o/r",
        "",
        "",
    );
    let exec = JobExecutor {
        job: &j,
        repo_root: tmp.path().to_path_buf(),
        work_dir: tmp.path().join("work"),
        init_lock: None,
        isolate_data_dir: false,
        cli_config: None,
    };
    let err = exec.plan().await.expect_err("must fail");
    assert_eq!(err.step, "init");
}

const NULL_PROVIDER_TF: &str = r#"
terraform {
  required_providers {
    null = { source = "hashicorp/null" }
  }
}
resource "null_resource" "demo" {}
"#;

#[tokio::test]
async fn cli_config_is_written_and_passed_to_terraform() {
    let f = fixture(DEMO_TF, "    plan:\n      steps: [init, plan]\n");
    let plan_job = job(&f, Command::Plan);
    let mut exec = executor(&f, &plan_job);
    assert_eq!(exec.cli_config_file().unwrap(), None);
    std::fs::create_dir_all(&exec.work_dir).unwrap();
    exec.cli_config = Some("provider_installation {}\n".into());
    let path = exec.cli_config_file().unwrap().expect("path");
    assert_eq!(path, f.root.join("work/cli-config.tfrc"));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "provider_installation {}\n"
    );
}

#[tokio::test]
async fn provider_outside_the_allowlist_fails_init() {
    require_terraform!();
    let f = fixture(NULL_PROVIDER_TF, "    plan:\n      steps: [init, plan]\n");
    let plan_job = job(&f, Command::Plan);
    let mut exec = executor(&f, &plan_job);
    exec.cli_config = Some(
        providers::cli_config(&["registry.terraform.io/hashicorp/aws".to_string()], None).unwrap(),
    );
    let err = exec.plan().await.expect_err("init must fail");
    assert_eq!(err.step, "init");
    assert!(
        err.message.contains("allowed-providers is set"),
        "{}",
        err.message
    );
    let text = format!("{}\n{}", err.stdout, err.stderr);
    assert!(text.contains("hashicorp/null"), "{}", text);
}
