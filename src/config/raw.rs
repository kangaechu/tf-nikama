//! Raw YAML representation of `nikama.yml`.
//!
//! Every struct uses `deny_unknown_fields`, so unknown keys are rejected by
//! serde. Keys that exist in Digger's `digger.yml` but are unsupported by this
//! tool are declared as `Option<Value>` so that they can be reported with a
//! dedicated "unsupported" error instead of a generic unknown-field error.

use serde::Deserialize;
use serde_yaml::Value;
use std::collections::BTreeMap;

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RawConfig {
    #[serde(default)]
    pub projects: Vec<RawProject>,
    #[serde(default)]
    pub workflows: BTreeMap<String, Option<RawWorkflow>>,
    pub generate_projects: Option<RawGenerateProjects>,
    pub dependency_configuration: Option<RawDependencyConfiguration>,
    pub traverse_to_nested_projects: Option<bool>,
    pub auto_merge: Option<bool>,
    pub auto_merge_strategy: Option<String>,
    pub pr_locks: Option<bool>,
    pub allow_draft_prs: Option<bool>,
    pub git_timeout: Option<i64>,
    pub telemetry: Option<bool>,
    #[serde(default)]
    pub drift_include_patterns: Vec<String>,
    #[serde(default)]
    pub drift_exclude_patterns: Vec<String>,
    pub concurrency: Option<RawConcurrency>,
    pub comment: Option<RawComment>,

    // ---- unsupported keys (backend-only or unused in Digger) ----
    pub respect_layers: Option<Value>,
    pub delete_prior_comments: Option<Value>,
    pub disable_digger_apply_comment: Option<Value>,
    pub disable_digger_apply_status_check: Option<Value>,
    pub comment_render_mode: Option<Value>,
    #[serde(rename = "trusted_appIDs")]
    pub trusted_app_ids: Option<Value>,
    pub report_terraform_outputs: Option<Value>,
    pub reporting: Option<Value>,
    pub apply_after_merge: Option<Value>,
    pub mention_drifted_projects_in_pr: Option<Value>,
}

impl RawConfig {
    pub fn unsupported_keys(&self) -> Vec<String> {
        let mut keys = Vec::new();
        let checks: [(&str, bool); 10] = [
            ("respect_layers", self.respect_layers.is_some()),
            (
                "delete_prior_comments",
                self.delete_prior_comments.is_some(),
            ),
            (
                "disable_digger_apply_comment",
                self.disable_digger_apply_comment.is_some(),
            ),
            (
                "disable_digger_apply_status_check",
                self.disable_digger_apply_status_check.is_some(),
            ),
            ("comment_render_mode", self.comment_render_mode.is_some()),
            ("trusted_appIDs", self.trusted_app_ids.is_some()),
            (
                "report_terraform_outputs",
                self.report_terraform_outputs.is_some(),
            ),
            ("reporting", self.reporting.is_some()),
            ("apply_after_merge", self.apply_after_merge.is_some()),
            (
                "mention_drifted_projects_in_pr",
                self.mention_drifted_projects_in_pr.is_some(),
            ),
        ];
        for (k, set) in checks {
            if set {
                keys.push(k.to_string());
            }
        }
        for (i, p) in self.projects.iter().enumerate() {
            for k in p.unsupported_keys() {
                keys.push(format!("projects[{}].{}", i, k));
            }
        }
        if let Some(g) = &self.generate_projects {
            for k in g.unsupported_keys() {
                keys.push(format!("generate_projects.{}", k));
            }
        }
        keys
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RawDependencyConfiguration {
    pub mode: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RawConcurrency {
    pub max_parallel: Option<usize>,
    pub apply_parallelism: Option<usize>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RawComment {
    pub mode: Option<String>,
    pub show_empty_plans: Option<String>,
    pub per_project_status: Option<bool>,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct RawProject {
    pub name: String,
    pub dir: String,
    pub workspace: Option<String>,
    pub workflow: Option<String>,
    pub opentofu: Option<bool>,
    #[serde(default)]
    pub include_patterns: Vec<String>,
    #[serde(default)]
    pub exclude_patterns: Vec<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    pub layer: Option<u32>,
    pub alias: Option<String>,
    pub block_name: Option<String>,
    pub branch: Option<String>,
    pub apply_requirements: Option<Vec<String>>,
    pub drift_detection: Option<bool>,
    #[serde(default)]
    pub drift_include_patterns: Vec<String>,
    #[serde(default)]
    pub drift_exclude_patterns: Vec<String>,
    pub aws_role_to_assume: Option<RawAwsRoleToAssume>,
    pub aws_cognito_oidc: Option<RawAwsCognitoOidc>,

    // ---- unsupported ----
    pub terragrunt: Option<Value>,
    pub pulumi: Option<Value>,
    pub pulumi_stack: Option<Value>,
    pub workflow_file: Option<Value>,
    pub generated: Option<Value>,
    pub drift_terragrunt_parallelism: Option<Value>,
}

impl RawProject {
    pub fn unsupported_keys(&self) -> Vec<&'static str> {
        let mut keys = Vec::new();
        if self.terragrunt.is_some() {
            keys.push("terragrunt");
        }
        if self.pulumi.is_some() {
            keys.push("pulumi");
        }
        if self.pulumi_stack.is_some() {
            keys.push("pulumi_stack");
        }
        if self.workflow_file.is_some() {
            keys.push("workflow_file");
        }
        if self.generated.is_some() {
            keys.push("generated");
        }
        if self.drift_terragrunt_parallelism.is_some() {
            keys.push("drift_terragrunt_parallelism");
        }
        keys
    }
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct RawAwsRoleToAssume {
    pub aws_role_region: Option<String>,
    pub state: Option<String>,
    pub command: Option<String>,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct RawAwsCognitoOidc {
    pub aws_account_id: Option<String>,
    pub aws_region: Option<String>,
    pub cognito_identity_pool_id: Option<String>,
    pub session_duration: Option<u32>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RawWorkflow {
    pub env_vars: Option<RawEnvVars>,
    pub plan: Option<RawStage>,
    pub apply: Option<RawStage>,
    pub workflow_configuration: Option<RawWorkflowConfiguration>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RawEnvVars {
    #[serde(default)]
    pub state: Vec<RawEnvVar>,
    #[serde(default)]
    pub commands: Vec<RawEnvVar>,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct RawEnvVar {
    pub name: String,
    pub value: Option<String>,
    pub value_from: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RawStage {
    pub filter_regex: Option<String>,
    #[serde(default)]
    pub steps: Vec<RawStep>,
}

/// A step accepts three YAML shapes:
///
/// ```yaml
/// - init
/// - run: "echo hi"
///   shell: bash
/// - plan:
///     extra_args: ["-var-file=x.tfvars"]
/// - init
///   extra_args: ["-upgrade"]
/// ```
#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum RawStep {
    Name(String),
    Map(serde_yaml::Mapping),
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RawWorkflowConfiguration {
    pub on_pull_request_pushed: Option<Vec<String>>,
    pub on_pull_request_closed: Option<Vec<String>>,
    pub on_commit_to_default: Option<Vec<String>>,
    pub on_pull_request_to_draft: Option<Vec<String>>,
    pub skip_merge_check: Option<bool>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RawGenerateProjects {
    pub include: Option<String>,
    pub exclude: Option<String>,
    #[serde(default)]
    pub blocks: Vec<RawBlock>,
    pub aws_role_to_assume: Option<RawAwsRoleToAssume>,
    pub aws_cognito_oidc: Option<RawAwsCognitoOidc>,
    #[serde(default)]
    pub drift_include_patterns: Vec<String>,
    #[serde(default)]
    pub drift_exclude_patterns: Vec<String>,

    // ---- unsupported ----
    pub terragrunt: Option<Value>,
    pub terragrunt_parsing: Option<Value>,
    pub drift_terragrunt_parallelism: Option<Value>,
}

impl RawGenerateProjects {
    pub fn unsupported_keys(&self) -> Vec<String> {
        let mut keys = Vec::new();
        if self.terragrunt.is_some() {
            keys.push("terragrunt".to_string());
        }
        if self.terragrunt_parsing.is_some() {
            keys.push("terragrunt_parsing".to_string());
        }
        if self.drift_terragrunt_parallelism.is_some() {
            keys.push("drift_terragrunt_parallelism".to_string());
        }
        for (i, b) in self.blocks.iter().enumerate() {
            for k in b.unsupported_keys() {
                keys.push(format!("blocks[{}].{}", i, k));
            }
        }
        keys
    }
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct RawBlock {
    pub block_name: Option<String>,
    pub include: Option<String>,
    pub exclude: Option<String>,
    #[serde(default)]
    pub include_patterns: Vec<String>,
    #[serde(default)]
    pub exclude_patterns: Vec<String>,
    pub opentofu: Option<bool>,
    pub workflow: Option<String>,
    pub workspace: Option<String>,
    pub aws_role_to_assume: Option<RawAwsRoleToAssume>,
    pub aws_cognito_oidc: Option<RawAwsCognitoOidc>,
    #[serde(default)]
    pub drift_include_patterns: Vec<String>,
    #[serde(default)]
    pub drift_exclude_patterns: Vec<String>,

    // ---- unsupported ----
    pub terragrunt: Option<Value>,
    pub root_dir: Option<Value>,
    pub terragrunt_parsing: Option<Value>,
    pub workflow_file: Option<Value>,
    pub drift_terragrunt_parallelism: Option<Value>,
}

impl RawBlock {
    pub fn unsupported_keys(&self) -> Vec<&'static str> {
        let mut keys = Vec::new();
        if self.terragrunt.is_some() {
            keys.push("terragrunt");
        }
        if self.root_dir.is_some() {
            keys.push("root_dir");
        }
        if self.terragrunt_parsing.is_some() {
            keys.push("terragrunt_parsing");
        }
        if self.workflow_file.is_some() {
            keys.push("workflow_file");
        }
        if self.drift_terragrunt_parallelism.is_some() {
            keys.push("drift_terragrunt_parallelism");
        }
        keys
    }
}
