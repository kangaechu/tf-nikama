//! Configuration loading, validation, generation and impact tests.

use super::*;
use pretty_assertions::assert_eq;

fn load(yaml: &str) -> Result<Config, ConfigError> {
    let dir = tempfile::tempdir().unwrap();
    load_from_str(yaml, dir.path())
}

fn err_msg(r: Result<Config, ConfigError>) -> String {
    match r {
        Ok(_) => panic!("expected an error"),
        Err(e) => e.to_string(),
    }
}

#[test]
fn defaults_are_applied() {
    let c = load("projects:\n  - name: a\n    dir: infra/a\n").unwrap();
    let p = &c.projects[0];
    assert_eq!(p.workspace, "default");
    assert_eq!(p.workflow, "default");
    assert!(!p.opentofu);
    assert_eq!(p.apply_requirements, vec!["mergeable"]);
    assert!(p.drift_detection);
    assert_eq!(p.layer, 0);
    let wf = &c.workflows["default"];
    assert_eq!(wf.plan, Stage::default_plan());
    assert_eq!(wf.apply, Stage::default_apply());
    assert_eq!(wf.configuration.on_pull_request_pushed, vec!["nikama plan"]);
    assert_eq!(
        wf.configuration.on_pull_request_closed,
        vec!["nikama unlock"]
    );
    assert_eq!(c.pr_locks, LockMode::OnPlan);
    assert_eq!(c.no_lock_label, None);
    assert!(!c.auto_merge);
    assert_eq!(c.auto_merge_strategy, MergeStrategy::Squash);
    assert_eq!(c.dependency_mode, DependencyMode::Hard);
    assert_eq!(c.on_failure, OnFailure::SkipDependents);
    assert!(c.traverse_to_nested_projects);
    assert_eq!(c.concurrency.max_parallel, 4);
    assert_eq!(c.concurrency.apply_parallelism, 1);
    assert_eq!(c.timezone, chrono_tz::UTC);
    assert_eq!(c.comment.mode, CommentMode::Single);
    assert!(!c.comment.mode_explicit);
    assert_eq!(c.comment.show_empty_plans, ShowEmptyPlans::Table);
}

#[test]
fn comment_mode_is_flagged_when_written_and_maps_from_reporting_strategy() {
    let c = load("projects: []\ncomment:\n  mode: single\n").unwrap();
    assert_eq!(c.comment.mode, CommentMode::Single);
    assert!(c.comment.mode_explicit);
    let c = load("projects: []\ncomment:\n  show_empty_plans: hide\n").unwrap();
    assert!(!c.comment.mode_explicit);

    // Digger's reporting-strategy names.
    assert_eq!(
        CommentMode::from_reporting_strategy("comments_per_run"),
        Some(CommentMode::Single)
    );
    assert_eq!(
        CommentMode::from_reporting_strategy(" Per_Project "),
        Some(CommentMode::PerProject)
    );
    assert_eq!(
        CommentMode::from_reporting_strategy("latest_run_comment"),
        Some(CommentMode::Latest)
    );
    assert_eq!(CommentMode::from_reporting_strategy("single"), None);
    assert_eq!(CommentMode::from_reporting_strategy(""), None);
}

#[test]
fn all_three_step_forms_parse() {
    let c = load(
        r#"
projects:
  - name: a
    dir: a
    workflow: w
workflows:
  w:
    plan:
      filter_regex: "secret-\\w+"
      steps:
        - init
        - plan:
            extra_args: ["-var-file=x.tfvars", "-lock=false"]
        - run: "echo hi"
          shell: zsh
    apply:
      steps:
        - init:
          extra_args: ["-upgrade"]
        - apply
"#,
    )
    .unwrap();
    let wf = &c.workflows["w"];
    assert_eq!(wf.plan.filter_regex.as_deref(), Some("secret-\\w+"));
    assert_eq!(
        wf.plan.steps,
        vec![
            Step::Init { extra_args: vec![] },
            Step::Plan {
                extra_args: vec!["-var-file=x.tfvars".into(), "-lock=false".into()]
            },
            Step::Run {
                command: "echo hi".into(),
                shell: Some("zsh".into())
            },
        ]
    );
    assert_eq!(
        wf.apply.steps,
        vec![
            Step::Init {
                extra_args: vec!["-upgrade".into()]
            },
            Step::Apply { extra_args: vec![] },
        ]
    );
}

#[test]
fn step_errors() {
    let bad_action =
        load("projects: []\nworkflows:\n  w:\n    plan:\n      steps:\n        - destroy\n");
    assert!(err_msg(bad_action).contains("unknown step action 'destroy'"));
    let bad_key = load("projects: []\nworkflows:\n  w:\n    plan:\n      steps:\n        - plan:\n            foo: 1\n");
    assert!(err_msg(bad_key).contains("unknown key"));
    let empty = load("projects: []\nworkflows:\n  w:\n    plan:\n      steps: []\n");
    assert!(err_msg(empty).contains("at least one step"));
    let bad_regex = load("projects: []\nworkflows:\n  w:\n    plan:\n      filter_regex: \"(\"\n      steps: [init]\n");
    assert!(err_msg(bad_regex).contains("not a valid regex"));
}

#[test]
fn workflow_configuration_fills_missing_keys() {
    let c = load(
        "projects: []\nworkflows:\n  w:\n    workflow_configuration:\n      on_pull_request_pushed: [\"digger plan\", \"digger lock\"]\n      skip_merge_check: true\n",
    )
    .unwrap();
    let cfg = &c.workflows["w"].configuration;
    assert_eq!(
        cfg.on_pull_request_pushed,
        vec!["digger plan", "digger lock"]
    );
    assert_eq!(cfg.on_pull_request_closed, vec!["nikama unlock"]);
    assert_eq!(cfg.on_commit_to_default, vec!["nikama unlock"]);
    assert!(cfg.on_pull_request_to_draft.is_empty());
    assert!(cfg.skip_merge_check);
}

#[test]
fn workflow_configuration_rejects_destroy() {
    let r = load("projects: []\nworkflows:\n  w:\n    workflow_configuration:\n      on_pull_request_pushed: [\"digger destroy\"]\n");
    let err = format!("{}", r.unwrap_err());
    assert!(err.contains("cannot run 'digger destroy'"), "{}", err);
}

#[test]
fn workflow_configuration_rejects_unknown_commands() {
    let r = load("projects: []\nworkflows:\n  w:\n    workflow_configuration:\n      on_pull_request_pushed: [\"digger noop\"]\n");
    assert!(err_msg(r).contains("unknown command 'digger noop'"));
}

#[test]
fn null_workflow_is_the_default_workflow() {
    let c = load("projects: []\nworkflows:\n  default: null\n  custom:\n").unwrap();
    assert_eq!(c.workflows["default"], Workflow::default());
    assert_eq!(c.workflows["custom"], Workflow::default());
}

#[test]
fn env_vars_resolve_value_and_value_from() {
    let _g = crate::util::test_env_guard();
    std::env::set_var("NIKAMA_TEST_SECRET", "s3cr3t");
    let c = load(
        "projects: []\nworkflows:\n  w:\n    env_vars:\n      state:\n        - name: AWS_REGION\n          value: eu-west-1\n      commands:\n        - name: TF_VAR_x\n          value_from: NIKAMA_TEST_SECRET\n        - name: MISSING\n          value_from: NIKAMA_TEST_DOES_NOT_EXIST\n",
    )
    .unwrap();
    let (state, commands) = c.workflows["w"].env_vars.resolve();
    assert_eq!(state["AWS_REGION"], "eu-west-1");
    assert_eq!(commands["TF_VAR_x"], "s3cr3t");
    assert_eq!(commands["MISSING"], "");
    std::env::remove_var("NIKAMA_TEST_SECRET");
}

#[test]
fn unsupported_keys_are_reported_together() {
    let r = load(
        r#"
respect_layers: true
projects:
  - name: a
    dir: a
    terragrunt: true
    pulumi: true
generate_projects:
  terragrunt_parsing: {}
  blocks:
    - block_name: b
      include: "x/**"
      root_dir: x
"#,
    );
    let msg = err_msg(r);
    assert!(msg.contains("unsupported configuration key(s)"), "{}", msg);
    for k in [
        "respect_layers",
        "projects[0].terragrunt",
        "projects[0].pulumi",
        "generate_projects.terragrunt_parsing",
        "generate_projects.blocks[0].root_dir",
    ] {
        assert!(msg.contains(k), "missing {} in {}", k, msg);
    }
}

#[test]
fn unknown_keys_are_rejected() {
    let msg = err_msg(load("projects: []\nfoo: 1\n"));
    assert!(msg.contains("unknown field `foo`"), "{}", msg);
    let msg = err_msg(load("projects:\n  - name: a\n    dir: a\n    dirr: b\n"));
    assert!(msg.contains("unknown field `dirr`"), "{}", msg);
}

#[test]
fn project_validation_errors() {
    assert!(err_msg(load(
        "projects:\n  - name: a\n    dir: a\n  - name: a\n    dir: b\n"
    ))
    .contains("duplicate project name 'a'"));
    assert!(err_msg(load(
        "projects:\n  - name: a\n    dir: a\n    workflow: nope\n"
    ))
    .contains("workflow 'nope' is not defined"));
    assert!(err_msg(load(
        "projects:\n  - name: a\n    dir: a\n    depends_on: [zzz]\n"
    ))
    .contains("unknown project 'zzz'"));
    assert!(err_msg(load(
        "projects:\n  - name: a\n    dir: a\n    depends_on: [a]\n"
    ))
    .contains("cannot depend on itself"));
    assert!(err_msg(load(
        "projects:\n  - name: a\n    dir: a\n    depends_on: [b]\n  - name: b\n    dir: b\n    depends_on: [a]\n"
    ))
    .contains("cycle"));
    assert!(err_msg(load(
        "projects:\n  - name: a\n    dir: a\n    apply_requirements: [approved, bogus]\n"
    ))
    .contains("invalid apply_requirements value 'bogus'"));
    assert!(err_msg(load(
        "projects:\n  - name: a\n    dir: a\n    apply_requirements: [approved, approved]\n"
    ))
    .contains("duplicate apply_requirements"));
    assert!(
        err_msg(load("projects:\n  - name: ''\n    dir: a\n")).contains("name must not be empty")
    );
    assert!(
        err_msg(load("projects:\n  - name: a\n    dir: ''\n")).contains("dir must not be empty")
    );
}

#[test]
fn top_level_enum_validation() {
    assert!(err_msg(load(
        "projects: []\ndependency_configuration:\n  mode: medium\n"
    ))
    .contains("hard"));
    assert!(err_msg(load(
        "projects: []\ndependency_configuration:\n  on_failure: stop\n"
    ))
    .contains("skip_dependents"));
    assert!(err_msg(load("projects: []\nauto_merge_strategy: fast-forward\n")).contains("squash"));
    assert!(err_msg(load("projects: []\ncomment:\n  mode: many\n")).contains("single"));
    assert!(err_msg(load("projects: []\nconcurrency:\n  max_parallel: 0\n")).contains(">= 1"));
    let c = load("projects: []\ndependency_configuration:\n  mode: soft\n  on_failure: abort\nauto_merge: true\nauto_merge_strategy: rebase\ncomment:\n  mode: latest\n  show_empty_plans: hide\n").unwrap();
    assert_eq!(c.dependency_mode, DependencyMode::Soft);
    assert_eq!(c.on_failure, OnFailure::Abort);
    assert_eq!(
        load("projects: []\ndependency_configuration:\n  on_failure: continue\n")
            .unwrap()
            .on_failure,
        OnFailure::Continue
    );
    assert_eq!(c.auto_merge_strategy, MergeStrategy::Rebase);
    assert_eq!(c.comment.mode, CommentMode::Latest);
    assert_eq!(c.comment.show_empty_plans, ShowEmptyPlans::Hide);
}

#[test]
fn aws_role_copies_missing_side_and_defaults_region() {
    let c = load(
        "projects:\n  - name: a\n    dir: a\n    aws_role_to_assume:\n      state: arn:aws:iam::1:role/s\n",
    )
    .unwrap();
    let r = c.projects[0].aws_role_to_assume.clone().unwrap();
    assert_eq!(r.region, "us-east-1");
    assert_eq!(r.state.as_deref(), Some("arn:aws:iam::1:role/s"));
    assert_eq!(r.command.as_deref(), Some("arn:aws:iam::1:role/s"));
}

#[test]
fn aws_role_requires_init_first_in_both_stages() {
    let r = load(
        "projects:\n  - name: a\n    dir: a\n    workflow: w\n    aws_role_to_assume:\n      command: arn:x\nworkflows:\n  w:\n    plan:\n      steps: [plan]\n",
    );
    assert!(err_msg(r).contains("first plan step"));
}

#[test]
fn cognito_requires_fields() {
    let r = load(
        "projects:\n  - name: a\n    dir: a\n    aws_cognito_oidc:\n      aws_region: us-east-1\n",
    );
    assert!(err_msg(r).contains("aws_cognito_oidc.aws_account_id is required"));
}

fn make_tree(root: &Path, dirs: &[&str]) {
    for d in dirs {
        std::fs::create_dir_all(root.join(d)).unwrap();
        std::fs::write(root.join(d).join("main.tf"), "").unwrap();
    }
}

#[test]
fn generate_projects_with_blocks() {
    let tmp = tempfile::tempdir().unwrap();
    make_tree(
        tmp.path(),
        &[
            "infra/dev/vpc",
            "infra/dev/app",
            "infra/prod/vpc",
            "services/api",
            "modules/x",
        ],
    );
    let c = load_from_str(
        r#"
projects:
  - name: explicit
    dir: infra/prod/vpc
generate_projects:
  blocks:
    - block_name: dev
      include: "infra/dev/**"
      exclude: "infra/dev/app"
      workflow: devwf
      workspace: dev
    - block_name: svc
      include: "services/**"
      opentofu: true
      include_patterns: ["./*.tf", "shared/**"]
workflows:
  devwf:
"#,
        tmp.path(),
    )
    .unwrap();
    let names: Vec<&str> = c.projects.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec!["explicit", "infra_dev_vpc", "services_api"]);
    let dev = c.project("infra_dev_vpc").unwrap();
    assert_eq!(dev.workflow, "devwf");
    assert_eq!(dev.workspace, "dev");
    assert_eq!(dev.block_name.as_deref(), Some("dev"));
    assert!(dev.generated);
    let svc = c.project("services_api").unwrap();
    assert!(svc.opentofu);
    assert_eq!(svc.include_patterns, vec!["./*.tf", "shared/**"]);
}

#[test]
fn generate_projects_top_level_include() {
    let tmp = tempfile::tempdir().unwrap();
    make_tree(tmp.path(), &["a", "b/c"]);
    let c = load_from_str(
        "generate_projects:\n  include: \"**\"\n  exclude: \"b/**\"\n",
        tmp.path(),
    )
    .unwrap();
    let names: Vec<&str> = c.projects.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec!["a"]);
}

#[test]
fn generate_projects_validation() {
    let tmp = tempfile::tempdir().unwrap();
    let both = load_from_str(
        "generate_projects:\n  include: \"**\"\n  blocks:\n    - include: \"a/**\"\n",
        tmp.path(),
    );
    assert!(err_msg(both).contains("not both"));
    let no_include = load_from_str(
        "generate_projects:\n  blocks:\n    - block_name: x\n",
        tmp.path(),
    );
    assert!(err_msg(no_include).contains("include is required"));
}

#[test]
fn nested_projects_respect_traverse_flag() {
    let tmp = tempfile::tempdir().unwrap();
    make_tree(tmp.path(), &["root", "root/child"]);
    let all = load_from_str("generate_projects:\n  include: \"**\"\n", tmp.path()).unwrap();
    assert_eq!(all.projects.len(), 2);
    let top = load_from_str(
        "traverse_to_nested_projects: false\ngenerate_projects:\n  include: \"**\"\n",
        tmp.path(),
    )
    .unwrap();
    assert_eq!(top.projects.len(), 1);
    assert_eq!(top.projects[0].dir, "root");
}

#[test]
fn config_file_discovery_order() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    assert!(matches!(
        find_config_file(root, None),
        Err(ConfigError::NotFound(_))
    ));

    std::fs::write(root.join("digger.yml"), "projects: []\n").unwrap();
    let (p, legacy) = find_config_file(root, None).unwrap();
    assert!(p.ends_with("digger.yml"));
    assert!(legacy);

    std::fs::write(root.join("nikama.yml"), "projects: []\n").unwrap();
    let (p, legacy) = find_config_file(root, None).unwrap();
    assert!(p.ends_with("nikama.yml"));
    assert!(!legacy);

    std::fs::write(root.join("nikama.yaml"), "projects: []\n").unwrap();
    assert!(matches!(
        find_config_file(root, None),
        Err(ConfigError::Conflict(_, _))
    ));

    let (p, legacy) = find_config_file(root, Some(Path::new("digger.yml"))).unwrap();
    assert!(p.ends_with("digger.yml"));
    assert!(!legacy);
    assert!(matches!(
        find_config_file(root, Some(Path::new("missing.yml"))),
        Err(ConfigError::NotFound(_))
    ));
}

#[test]
fn load_reads_and_flags_legacy_name() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("digger.yml"),
        "projects:\n  - name: a\n    dir: a\n",
    )
    .unwrap();
    let l = super::load(tmp.path(), None).unwrap();
    assert!(l.legacy_name);
    assert_eq!(l.config.projects[0].name, "a");
}

fn impact_config(mode: &str) -> Config {
    load(&format!(
        r#"
dependency_configuration:
  mode: {mode}
projects:
  - name: net
    dir: infra/net
  - name: app
    dir: infra/app
    depends_on: [net]
    include_patterns: ["./*.tfvars", "shared/**"]
    exclude_patterns: ["./README.md"]
  - name: docs
    dir: docs
  - name: other
    dir: other
    depends_on: [app]
"#
    ))
    .unwrap()
}

fn names(ps: &[Project]) -> Vec<&str> {
    ps.iter().map(|p| p.name.as_str()).collect()
}

#[test]
fn impacted_direct_matches() {
    let c = impact_config("soft");
    assert_eq!(
        names(&impacted_projects(&c, &["infra/net/main.tf".into()], None)),
        vec!["net"]
    );
    assert_eq!(
        names(&impacted_projects(&c, &["infra/app/sub/x.tf".into()], None)),
        vec!["app"]
    );
    // include_patterns relative to dir and repository-relative.
    assert_eq!(
        names(&impacted_projects(
            &c,
            &["infra/app/prod.tfvars".into()],
            None
        )),
        vec!["app"]
    );
    assert_eq!(
        names(&impacted_projects(&c, &["shared/vars.tf".into()], None)),
        vec!["app"]
    );
    // exclude wins.
    assert!(impacted_projects(&c, &["infra/app/README.md".into()], None).is_empty());
    assert!(impacted_projects(&c, &["unrelated.txt".into()], None).is_empty());
}

#[test]
fn impacted_hard_mode_adds_dependents_in_config_order() {
    let c = impact_config("hard");
    assert_eq!(
        names(&impacted_projects(&c, &["infra/net/main.tf".into()], None)),
        vec!["net", "app", "other"]
    );
    assert_eq!(
        names(&impacted_projects(&c, &["docs/x.tf".into()], None)),
        vec!["docs"]
    );
    assert_eq!(
        names(&impacted_projects(
            &c,
            &["other/x.tf".into(), "infra/app/a.tf".into()],
            None
        )),
        vec!["app", "other"]
    );
}

#[test]
fn run_steps_are_listed_and_can_be_forbidden() {
    let c = load(
        "projects:\n  - name: a\n    dir: a\n    workflow: w\nworkflows:\n  w:\n    plan:\n      steps:\n        - init\n        - plan\n        - run: echo hi\n    apply:\n      steps:\n        - run: echo pre\n        - init\n        - apply\n",
    )
    .unwrap();
    assert_eq!(
        c.run_steps(),
        vec!["workflow 'w' plan.steps[2]", "workflow 'w' apply.steps[0]"]
    );
    assert!(c.check_run_steps(true).is_ok());
    let msg = c.check_run_steps(false).unwrap_err().to_string();
    assert!(msg.contains("allow-run-steps is false"), "{}", msg);
    assert!(msg.contains("workflow 'w' plan.steps[2]"), "{}", msg);

    let plain = load("projects:\n  - name: a\n    dir: a\n").unwrap();
    assert!(plain.run_steps().is_empty());
    assert!(plain.check_run_steps(false).is_ok());
}

#[test]
fn allowed_providers_are_normalised_and_validated() {
    let c = load(
        "projects:\n  - name: a\n    dir: a\nallowed_providers:\n  - hashicorp/AWS\n  - registry.terraform.io/hashicorp/aws\n  - app.terraform.io/org/*\n",
    )
    .unwrap();
    assert_eq!(
        c.allowed_providers,
        vec![
            "registry.terraform.io/hashicorp/aws",
            "app.terraform.io/org/*"
        ]
    );
    let msg = err_msg(load(
        "projects:\n  - name: a\n    dir: a\nallowed_providers: [aws]\n",
    ));
    assert!(msg.contains("allowed_providers"), "{}", msg);
    assert!(msg.contains("invalid provider pattern 'aws'"), "{}", msg);
    assert!(load("projects:\n  - name: a\n    dir: a\n")
        .unwrap()
        .allowed_providers
        .is_empty());
}

#[test]
fn no_lock_label_is_trimmed_and_optional() {
    let dir = tempfile::tempdir().unwrap();
    let c = load_from_str(
        "projects:\n  - name: a\n    dir: a\nno_lock_label: '  no-lock '\n",
        dir.path(),
    )
    .unwrap();
    assert_eq!(c.no_lock_label.as_deref(), Some("no-lock"));
    let c = load_from_str(
        "projects:\n  - name: a\n    dir: a\nno_lock_label: ''\n",
        dir.path(),
    )
    .unwrap();
    assert_eq!(c.no_lock_label, None);
}

#[test]
fn wakeup_word_defaults_to_nikama_and_digger() {
    let c = load("projects: []\n").unwrap();
    assert_eq!(c.wakeup_words, vec!["nikama", "digger"]);
    assert_eq!(c.wakeup_word(), "nikama");
}

#[test]
fn wakeup_word_accepts_a_string_or_a_list() {
    let c = load("projects: []\nwakeup_word: tf\n").unwrap();
    assert_eq!(c.wakeup_words, vec!["tf"]);
    assert_eq!(c.wakeup_word(), "tf");

    let c = load("projects: []\nwakeup_word: [' Terraform ', nikama, TERRAFORM]\n").unwrap();
    // Trimmed, order kept, case-insensitive duplicates dropped.
    assert_eq!(c.wakeup_words, vec!["Terraform", "nikama"]);
    assert_eq!(c.wakeup_word(), "Terraform");
}

#[test]
fn wakeup_word_rejects_bad_values() {
    assert!(err_msg(load("projects: []\nwakeup_word: ''\n")).contains("must not be empty"));
    assert!(err_msg(load("projects: []\nwakeup_word: []\n")).contains("at least one word"));
    assert!(err_msg(load("projects: []\nwakeup_word: 'tf bot'\n"))
        .contains("must not contain whitespace"));
    assert!(err_msg(load("projects: []\nwakeup_word: '-p'\n")).contains("must not start with '-'"));
}

#[test]
fn workflow_configuration_accepts_the_configured_wakeup_word() {
    let c = load(
        "projects: []\nwakeup_word: tf\nworkflows:\n  w:\n    workflow_configuration:\n      on_pull_request_pushed: [\"tf plan\", \"digger lock\"]\n",
    )
    .unwrap();
    assert_eq!(
        c.workflows["w"].configuration.on_pull_request_pushed,
        vec!["tf plan", "digger lock"]
    );
    // The defaults (`nikama plan` / `nikama unlock`) and Digger-era entries
    // (`digger lock` above) stay valid whatever the word.
    assert_eq!(
        c.workflows["default"].configuration.on_pull_request_pushed,
        vec!["nikama plan"]
    );
    let r = load("projects: []\nworkflows:\n  w:\n    workflow_configuration:\n      on_pull_request_pushed: [\"tf plan\"]\n");
    assert!(err_msg(r).contains("unknown command 'tf plan'"));
}

#[test]
fn autoplan_modules_impacts_projects_through_local_modules() {
    let tmp = tempfile::tempdir().unwrap();
    make_tree(
        tmp.path(),
        &["infra/app", "infra/net", "modules/vpc", "modules/subnet"],
    );
    std::fs::write(
        tmp.path().join("infra/app/main.tf"),
        "module \"vpc\" {\n  source = \"../../modules/vpc\"\n}\n",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("modules/vpc/main.tf"),
        "module \"subnet\" {\n  source = \"../subnet\"\n}\n",
    )
    .unwrap();
    let yaml = "projects:\n  - name: app\n    dir: infra/app\n  - name: net\n    dir: infra/net\n";

    // Off by default: a module change impacts nothing.
    let c = load_from_str(yaml, tmp.path()).unwrap();
    assert!(!c.autoplan_modules);
    assert!(c.projects[0].module_dirs.is_empty());
    assert!(impacted_projects(&c, &["modules/subnet/main.tf".into()], None).is_empty());

    let c = load_from_str(&format!("{yaml}autoplan_modules: true\n"), tmp.path()).unwrap();
    assert!(c.autoplan_modules);
    assert_eq!(
        c.projects[0].module_dirs,
        vec!["modules/subnet", "modules/vpc"]
    );
    assert!(c.projects[1].module_dirs.is_empty());
    assert_eq!(
        names(&impacted_projects(
            &c,
            &["modules/subnet/main.tf".into()],
            None
        )),
        vec!["app"]
    );
    assert_eq!(
        names(&impacted_projects(
            &c,
            &["modules/vpc/README.md".into()],
            None
        )),
        vec!["app"]
    );
    assert!(impacted_projects(&c, &["modules/other/main.tf".into()], None).is_empty());
}

#[test]
fn autoplan_modules_applies_to_generated_projects() {
    let tmp = tempfile::tempdir().unwrap();
    make_tree(tmp.path(), &["services/api", "modules/svc"]);
    std::fs::write(
        tmp.path().join("services/api/main.tf"),
        "module \"svc\" {\n  source = \"../../modules/svc\"\n}\n",
    )
    .unwrap();
    let c = load_from_str(
        "generate_projects:\n  include: \"services/**\"\nautoplan_modules: true\n",
        tmp.path(),
    )
    .unwrap();
    assert_eq!(c.projects.len(), 1);
    assert_eq!(c.projects[0].module_dirs, vec!["modules/svc"]);
    assert_eq!(
        names(&impacted_projects(
            &c,
            &["modules/svc/main.tf".into()],
            None
        )),
        vec![c.projects[0].name.as_str()]
    );
}

#[test]
fn branch_is_a_regex_matched_against_the_base_branch() {
    let c = load(
        r#"
projects:
  - name: app-staging
    dir: infra/app
    workspace: staging
    branch: ^develop$
  - name: app-prod
    dir: infra/app
    workspace: prod
    branch: ^(main|release/.*)$
  - name: shared
    dir: infra/shared
  - name: downstream
    dir: infra/downstream
    depends_on: [shared]
    branch: ^main$
"#,
    )
    .unwrap();
    assert_eq!(c.projects[0].branch.as_ref().unwrap().as_str(), "^develop$");
    assert!(c.projects[2].branch.is_none());
    // Empty / blank values mean "no filter".
    let blank = load("projects:\n  - name: a\n    dir: a\n    branch: '  '\n").unwrap();
    assert!(blank.projects[0].branch.is_none());

    let files = vec!["infra/app/main.tf".to_string()];
    assert_eq!(
        names(&impacted_projects(&c, &files, Some("develop"))),
        vec!["app-staging"]
    );
    assert_eq!(
        names(&impacted_projects(&c, &files, Some("release/1.2"))),
        vec!["app-prod"]
    );
    assert!(impacted_projects(&c, &files, Some("feature/x")).is_empty());
    // No pull request (manual / drift): no filter.
    assert_eq!(
        names(&impacted_projects(&c, &files, None)),
        vec!["app-staging", "app-prod"]
    );
    // Downstream dependents are filtered too (hard mode is the default).
    let shared = vec!["infra/shared/main.tf".to_string()];
    assert_eq!(
        names(&impacted_projects(&c, &shared, Some("main"))),
        vec!["shared", "downstream"]
    );
    assert_eq!(
        names(&impacted_projects(&c, &shared, Some("develop"))),
        vec!["shared"]
    );
}

#[test]
fn branch_must_be_a_valid_regex() {
    let msg = err_msg(load(
        "projects:\n  - name: a\n    dir: a\n    branch: '(main'\n",
    ));
    assert!(
        msg.contains("project 'a': branch is not a valid regular expression"),
        "{msg}"
    );
}

#[test]
fn pr_locks_accepts_booleans_and_modes() {
    for (value, expected) in [
        ("true", LockMode::OnPlan),
        ("on_plan", LockMode::OnPlan),
        ("ON_APPLY", LockMode::OnApply),
        ("on_apply", LockMode::OnApply),
        ("false", LockMode::Disabled),
        ("disabled", LockMode::Disabled),
    ] {
        let c = load(&format!("projects: []\npr_locks: {value}\n")).unwrap();
        assert_eq!(c.pr_locks, expected, "{value}");
    }
    assert!(LockMode::OnApply.enabled());
    assert!(!LockMode::Disabled.enabled());
    let msg = err_msg(load("projects: []\npr_locks: sometimes\n"));
    assert!(
        msg.contains(
            "pr_locks must be true, false, on_plan, on_apply or disabled, got 'sometimes'"
        ),
        "{msg}"
    );
}

#[test]
fn allowed_plan_args_are_validated_and_matched() {
    let c = load("projects: []\n").unwrap();
    assert!(c.allowed_plan_args.is_empty());
    let c = load(
        "projects: []\nallowed_plan_args: [' -target=* ', '-replace=*', '-refresh=false', '-target=*']\n",
    )
    .unwrap();
    assert_eq!(
        c.allowed_plan_args,
        vec!["-target=*", "-replace=*", "-refresh=false"]
    );
    assert_eq!(c.check_extra_args(&[]), Ok(()));
    assert_eq!(
        c.check_extra_args(&["-target=module.a.aws_s3_bucket.b[\"x/y\"]".into()]),
        Ok(())
    );
    assert_eq!(
        c.check_extra_args(&["-refresh=false".into(), "-replace=aws_instance.web".into()]),
        Ok(())
    );
    assert_eq!(
        c.check_extra_args(&["-refresh=true".into()]),
        Err("-refresh=true")
    );
    // Values must be in the `-flag=value` form: a bare value is its own token.
    assert_eq!(
        c.check_extra_args(&["-target".into(), "module.a".into()]),
        Err("-target")
    );
    assert_eq!(
        c.check_extra_args(&["-target=x".into(), "-var=a=b".into()]),
        Err("-var=a=b")
    );

    for (yaml, msg) in [
        ("allowed_plan_args: ['']", "must not be empty"),
        ("allowed_plan_args: ['target=*']", "must start with '-'"),
        (
            "allowed_plan_args: ['-target=[']",
            "allowed_plan_args entry '-target=['",
        ),
    ] {
        let m = err_msg(load(&format!("projects: []\n{yaml}\n")));
        assert!(m.contains(msg), "{m}");
    }
}

#[test]
fn timezone_is_validated() {
    let c = load("timezone: Asia/Tokyo\nprojects: []\n").unwrap();
    assert_eq!(c.timezone, chrono_tz::Asia::Tokyo);
    for invalid in ["", "Asia/Invalid", "+09:00"] {
        let yaml = format!("timezone: '{}'\nprojects: []\n", invalid);
        assert!(err_msg(load(&yaml)).contains("timezone must be an IANA time zone"));
    }
}
