//! Conversion from the raw YAML model to the validated configuration.

use super::raw::*;
use super::*;
use std::collections::{BTreeMap, BTreeSet};

const VALID_APPLY_REQUIREMENTS: [&str; 3] = ["approved", "mergeable", "undiverged"];

pub(super) fn convert(raw: RawConfig, dir: &Path) -> Result<Config, ConfigError> {
    let dependency_mode = match raw
        .dependency_configuration
        .as_ref()
        .and_then(|d| d.mode.as_deref())
        .unwrap_or("hard")
    {
        "hard" => DependencyMode::Hard,
        "soft" => DependencyMode::Soft,
        other => {
            return Err(ConfigError::Validation(format!(
                "dependency_configuration.mode must be 'hard' or 'soft', got '{}'",
                other
            )))
        }
    };

    let auto_merge_strategy = match raw.auto_merge_strategy.as_deref().unwrap_or("squash") {
        "squash" => MergeStrategy::Squash,
        "merge" => MergeStrategy::Merge,
        "rebase" => MergeStrategy::Rebase,
        other => {
            return Err(ConfigError::Validation(format!(
                "auto_merge_strategy must be squash, merge or rebase, got '{}'",
                other
            )))
        }
    };

    let concurrency = {
        let c = raw.concurrency.unwrap_or_default();
        let conc = Concurrency {
            max_parallel: c.max_parallel.unwrap_or(4),
            apply_parallelism: c.apply_parallelism.unwrap_or(1),
        };
        if conc.max_parallel == 0 || conc.apply_parallelism == 0 {
            return Err(ConfigError::Validation(
                "concurrency values must be >= 1".to_string(),
            ));
        }
        conc
    };

    let comment = {
        let c = raw.comment.unwrap_or_default();
        CommentConfig {
            mode: match c.mode.as_deref().unwrap_or("single") {
                "single" => CommentMode::Single,
                "per_project" => CommentMode::PerProject,
                "latest" => CommentMode::Latest,
                other => {
                    return Err(ConfigError::Validation(format!(
                        "comment.mode must be single, per_project or latest, got '{}'",
                        other
                    )))
                }
            },
            show_empty_plans: match c.show_empty_plans.as_deref().unwrap_or("table") {
                "table" => ShowEmptyPlans::Table,
                "hide" => ShowEmptyPlans::Hide,
                "details" => ShowEmptyPlans::Details,
                other => {
                    return Err(ConfigError::Validation(format!(
                        "comment.show_empty_plans must be table, hide or details, got '{}'",
                        other
                    )))
                }
            },
            per_project_status: c.per_project_status.unwrap_or(false),
        }
    };

    // Workflows.
    let mut workflows: BTreeMap<String, Workflow> = BTreeMap::new();
    for (name, wf) in raw.workflows {
        let workflow = match wf {
            None => Workflow::default(),
            Some(w) => convert_workflow(&name, w)?,
        };
        workflows.insert(name, workflow);
    }
    workflows.entry(DEFAULT_WORKFLOW.to_string()).or_default();

    // Projects: explicit first, then generated.
    let traverse_to_nested_projects = raw.traverse_to_nested_projects.unwrap_or(true);
    let mut projects: Vec<Project> = Vec::new();
    for p in raw.projects {
        projects.push(convert_project(p)?);
    }
    let mut drift_include_patterns = raw.drift_include_patterns;
    let mut drift_exclude_patterns = raw.drift_exclude_patterns;
    if let Some(gen) = raw.generate_projects {
        drift_include_patterns.extend(gen.drift_include_patterns.iter().cloned());
        drift_exclude_patterns.extend(gen.drift_exclude_patterns.iter().cloned());
        let generated = generate::generate_projects(&gen, dir, traverse_to_nested_projects)?;
        projects.extend(generated);
    }

    validate_projects(&projects, &workflows)?;
    let graph = DependencyGraph::build(&projects)?;

    Ok(Config {
        projects,
        workflows,
        dependency_mode,
        traverse_to_nested_projects,
        auto_merge: raw.auto_merge.unwrap_or(false),
        auto_merge_strategy,
        pr_locks: raw.pr_locks.unwrap_or(true),
        allow_draft_prs: raw.allow_draft_prs.unwrap_or(false),
        drift_include_patterns,
        drift_exclude_patterns,
        concurrency,
        comment,
        graph,
    })
}

pub(super) fn convert_aws_role(r: &RawAwsRoleToAssume) -> AwsRoleToAssume {
    let state = r.state.clone().filter(|s| !s.is_empty());
    let command = r.command.clone().filter(|s| !s.is_empty());
    // If only one side is set, Digger copies it to the other.
    let (state, command) = match (state, command) {
        (Some(s), None) => (Some(s.clone()), Some(s)),
        (None, Some(c)) => (Some(c.clone()), Some(c)),
        other => other,
    };
    AwsRoleToAssume {
        region: r
            .aws_role_region
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "us-east-1".to_string()),
        state,
        command,
    }
}

pub(super) fn convert_cognito(
    r: &RawAwsCognitoOidc,
    ctx: &str,
) -> Result<AwsCognitoOidc, ConfigError> {
    let need = |v: &Option<String>, key: &str| -> Result<String, ConfigError> {
        v.clone().filter(|s| !s.is_empty()).ok_or_else(|| {
            ConfigError::Validation(format!("{}: aws_cognito_oidc.{} is required", ctx, key))
        })
    };
    Ok(AwsCognitoOidc {
        aws_account_id: need(&r.aws_account_id, "aws_account_id")?,
        aws_region: need(&r.aws_region, "aws_region")?,
        cognito_identity_pool_id: need(&r.cognito_identity_pool_id, "cognito_identity_pool_id")?,
        session_duration: r.session_duration,
    })
}

fn convert_project(p: RawProject) -> Result<Project, ConfigError> {
    if p.name.trim().is_empty() {
        return Err(ConfigError::Validation(
            "project name must not be empty".to_string(),
        ));
    }
    if p.dir.trim().is_empty() {
        return Err(ConfigError::Validation(format!(
            "project '{}': dir must not be empty",
            p.name
        )));
    }
    let apply_requirements = p
        .apply_requirements
        .unwrap_or_else(|| vec!["mergeable".to_string()]);
    let mut seen = BTreeSet::new();
    for r in &apply_requirements {
        if !VALID_APPLY_REQUIREMENTS.contains(&r.as_str()) {
            return Err(ConfigError::Validation(format!(
                "project '{}': invalid apply_requirements value '{}' (allowed: {})",
                p.name,
                r,
                VALID_APPLY_REQUIREMENTS.join(", ")
            )));
        }
        if !seen.insert(r.clone()) {
            return Err(ConfigError::Validation(format!(
                "project '{}': duplicate apply_requirements value '{}'",
                p.name, r
            )));
        }
    }
    let ctx = format!("project '{}'", p.name);
    Ok(Project {
        workspace: p
            .workspace
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_WORKSPACE.to_string()),
        workflow: p
            .workflow
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_WORKFLOW.to_string()),
        opentofu: p.opentofu.unwrap_or(false),
        include_patterns: p.include_patterns,
        exclude_patterns: p.exclude_patterns,
        depends_on: p.depends_on,
        layer: p.layer.unwrap_or(0),
        alias: p.alias.filter(|s| !s.is_empty()),
        block_name: p.block_name.filter(|s| !s.is_empty()),
        branch: p.branch.filter(|s| !s.is_empty()),
        apply_requirements,
        drift_detection: p.drift_detection.unwrap_or(true),
        drift_include_patterns: p.drift_include_patterns,
        drift_exclude_patterns: p.drift_exclude_patterns,
        aws_role_to_assume: p.aws_role_to_assume.as_ref().map(convert_aws_role),
        aws_cognito_oidc: p
            .aws_cognito_oidc
            .as_ref()
            .map(|c| convert_cognito(c, &ctx))
            .transpose()?,
        name: p.name,
        dir: p.dir,
        generated: false,
    })
}

fn convert_workflow(name: &str, w: RawWorkflow) -> Result<Workflow, ConfigError> {
    let env_vars = match w.env_vars {
        None => EnvVars::default(),
        Some(e) => EnvVars {
            state: e.state.into_iter().map(convert_env_var).collect(),
            commands: e.commands.into_iter().map(convert_env_var).collect(),
        },
    };
    let plan = match w.plan {
        None => Stage::default_plan(),
        Some(s) => convert_stage(name, "plan", s)?,
    };
    let apply = match w.apply {
        None => Stage::default_apply(),
        Some(s) => convert_stage(name, "apply", s)?,
    };
    let configuration = match w.workflow_configuration {
        None => WorkflowConfiguration::default(),
        Some(c) => {
            let d = WorkflowConfiguration::default();
            WorkflowConfiguration {
                on_pull_request_pushed: c
                    .on_pull_request_pushed
                    .unwrap_or(d.on_pull_request_pushed),
                on_pull_request_closed: c
                    .on_pull_request_closed
                    .unwrap_or(d.on_pull_request_closed),
                on_commit_to_default: c.on_commit_to_default.unwrap_or(d.on_commit_to_default),
                on_pull_request_to_draft: c
                    .on_pull_request_to_draft
                    .unwrap_or(d.on_pull_request_to_draft),
                skip_merge_check: c.skip_merge_check.unwrap_or(false),
            }
        }
    };
    for (field, cmds) in [
        (
            "on_pull_request_pushed",
            &configuration.on_pull_request_pushed,
        ),
        (
            "on_pull_request_closed",
            &configuration.on_pull_request_closed,
        ),
        ("on_commit_to_default", &configuration.on_commit_to_default),
        (
            "on_pull_request_to_draft",
            &configuration.on_pull_request_to_draft,
        ),
    ] {
        for c in cmds {
            if crate::engine::Command::parse(c).is_none() {
                return Err(ConfigError::Validation(format!(
                    "workflow '{}': workflow_configuration.{} contains unknown command '{}'",
                    name, field, c
                )));
            }
        }
    }
    Ok(Workflow {
        env_vars,
        plan,
        apply,
        configuration,
    })
}

fn convert_env_var(v: RawEnvVar) -> EnvVar {
    EnvVar {
        name: v.name,
        value: v.value,
        value_from: v.value_from,
    }
}

fn convert_stage(workflow: &str, stage: &str, s: RawStage) -> Result<Stage, ConfigError> {
    if let Some(re) = &s.filter_regex {
        regex::Regex::new(re).map_err(|e| {
            ConfigError::Validation(format!(
                "workflow '{}': {}.filter_regex is not a valid regex: {}",
                workflow, stage, e
            ))
        })?;
    }
    let mut steps = Vec::new();
    for (i, st) in s.steps.into_iter().enumerate() {
        steps.push(convert_step(st).map_err(|e| {
            ConfigError::Validation(format!(
                "workflow '{}': {}.steps[{}]: {}",
                workflow, stage, i, e
            ))
        })?);
    }
    if steps.is_empty() {
        return Err(ConfigError::Validation(format!(
            "workflow '{}': {} stage must have at least one step",
            workflow, stage
        )));
    }
    Ok(Stage {
        filter_regex: s.filter_regex,
        steps,
    })
}

fn convert_step(st: RawStep) -> Result<Step, String> {
    use serde_yaml::Value;
    match st {
        RawStep::Name(n) => action_step(&n, vec![]),
        RawStep::Map(map) => {
            let get_str = |k: &str| -> Option<String> {
                map.get(Value::String(k.to_string()))
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
            };
            let extra_args_from = |v: Option<&Value>| -> Result<Vec<String>, String> {
                match v {
                    None | Some(Value::Null) => Ok(vec![]),
                    Some(Value::Sequence(seq)) => seq
                        .iter()
                        .map(|x| match x {
                            Value::String(s) => Ok(s.clone()),
                            Value::Number(n) => Ok(n.to_string()),
                            Value::Bool(b) => Ok(b.to_string()),
                            other => Err(format!(
                                "extra_args entry must be a string, got {:?}",
                                other
                            )),
                        })
                        .collect(),
                    Some(other) => Err(format!("extra_args must be a list, got {:?}", other)),
                }
            };
            if map.contains_key(Value::String("run".to_string())) {
                let command = get_str("run").ok_or("run step value must be a string")?;
                let shell = get_str("shell");
                for k in map.keys() {
                    match k.as_str() {
                        Some("run") | Some("shell") => {}
                        other => return Err(format!("unknown key {:?} in run step", other)),
                    }
                }
                return Ok(Step::Run { command, shell });
            }
            for action in ["init", "plan", "apply"] {
                let key = Value::String(action.to_string());
                if let Some(v) = map.get(&key) {
                    // Either `init: {extra_args: [...]}` or `init:` with sibling `extra_args`.
                    let args = match v {
                        Value::Null => {
                            extra_args_from(map.get(Value::String("extra_args".to_string())))?
                        }
                        Value::Mapping(inner) => {
                            for k in inner.keys() {
                                if k.as_str() != Some("extra_args") {
                                    return Err(format!(
                                        "unknown key {:?} in {} step",
                                        k.as_str(),
                                        action
                                    ));
                                }
                            }
                            extra_args_from(inner.get(Value::String("extra_args".to_string())))?
                        }
                        other => {
                            return Err(format!(
                                "{} step must be a mapping, got {:?}",
                                action, other
                            ))
                        }
                    };
                    for k in map.keys() {
                        match k.as_str() {
                            Some(a) if a == action => {}
                            Some("extra_args") => {}
                            other => {
                                return Err(format!("unknown key {:?} in {} step", other, action))
                            }
                        }
                    }
                    return action_step(action, args);
                }
            }
            Err(format!(
                "step must be one of init/plan/apply/run, got keys {:?}",
                map.keys().filter_map(|k| k.as_str()).collect::<Vec<_>>()
            ))
        }
    }
}

fn action_step(action: &str, extra_args: Vec<String>) -> Result<Step, String> {
    match action {
        "init" => Ok(Step::Init { extra_args }),
        "plan" => Ok(Step::Plan { extra_args }),
        "apply" => Ok(Step::Apply { extra_args }),
        "" => Err("step action must not be empty".to_string()),
        other => Err(format!("unknown step action '{}'", other)),
    }
}

fn validate_projects(
    projects: &[Project],
    workflows: &BTreeMap<String, Workflow>,
) -> Result<(), ConfigError> {
    let mut names = BTreeSet::new();
    for p in projects {
        if !names.insert(p.name.clone()) {
            return Err(ConfigError::Validation(format!(
                "duplicate project name '{}'",
                p.name
            )));
        }
    }
    for p in projects {
        let wf = workflows.get(&p.workflow).ok_or_else(|| {
            ConfigError::Validation(format!(
                "project '{}': workflow '{}' is not defined",
                p.name, p.workflow
            ))
        })?;
        for d in &p.depends_on {
            if !names.contains(d) {
                return Err(ConfigError::Validation(format!(
                    "project '{}': depends_on references unknown project '{}'",
                    p.name, d
                )));
            }
            if d == &p.name {
                return Err(ConfigError::Validation(format!(
                    "project '{}': cannot depend on itself",
                    p.name
                )));
            }
        }
        // The "backend-config" AWS strategy injects credentials into the first
        // init step, so it must exist.
        if p.aws_role_to_assume.is_some() && p.aws_cognito_oidc.is_none() {
            for (stage_name, stage) in [("plan", &wf.plan), ("apply", &wf.apply)] {
                if !stage.steps.first().map(|s| s.is_init()).unwrap_or(false) {
                    return Err(ConfigError::Validation(format!(
                        "project '{}': aws_role_to_assume requires the first {} step of workflow '{}' to be 'init'",
                        p.name, stage_name, p.workflow
                    )));
                }
            }
        }
    }
    Ok(())
}
