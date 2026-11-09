//! `nikama.yml` loading, validation and the in-memory configuration model.

mod convert;
mod generate;
mod graph;
mod impact;
mod raw;

pub use graph::DependencyGraph;
pub use impact::{impacted_projects, matches_patterns, normalize_path};

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const CONFIG_FILE_NAMES: [&str; 2] = ["nikama.yml", "nikama.yaml"];
pub const LEGACY_CONFIG_FILE_NAMES: [&str; 2] = ["digger.yml", "digger.yaml"];
pub const DEFAULT_WORKFLOW: &str = "default";
pub const DEFAULT_WORKSPACE: &str = "default";

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("no configuration file found (looked for {0})")]
    NotFound(String),
    #[error("both {0} and {1} exist; keep only one")]
    Conflict(String, String),
    #[error("failed to read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse {path}: {source}")]
    Yaml {
        path: PathBuf,
        #[source]
        source: serde_yaml::Error,
    },
    #[error("unsupported configuration key(s) in this tool: {}", .0.join(", "))]
    Unsupported(Vec<String>),
    #[error("invalid configuration: {0}")]
    Validation(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencyMode {
    Hard,
    Soft,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeStrategy {
    Squash,
    Merge,
    Rebase,
}

impl MergeStrategy {
    pub fn as_github_method(&self) -> &'static str {
        match self {
            MergeStrategy::Squash => "squash",
            MergeStrategy::Merge => "merge",
            MergeStrategy::Rebase => "rebase",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentMode {
    Single,
    PerProject,
    Latest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShowEmptyPlans {
    Table,
    Hide,
    Details,
}

#[derive(Debug, Clone)]
pub struct Concurrency {
    pub max_parallel: usize,
    pub apply_parallelism: usize,
}

impl Default for Concurrency {
    fn default() -> Self {
        Self {
            max_parallel: 4,
            apply_parallelism: 1,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CommentConfig {
    pub mode: CommentMode,
    pub show_empty_plans: ShowEmptyPlans,
    pub per_project_status: bool,
}

impl Default for CommentConfig {
    fn default() -> Self {
        Self {
            mode: CommentMode::Single,
            show_empty_plans: ShowEmptyPlans::Table,
            per_project_status: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AwsRoleToAssume {
    pub region: String,
    pub state: Option<String>,
    pub command: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AwsCognitoOidc {
    pub aws_account_id: String,
    pub aws_region: String,
    pub cognito_identity_pool_id: String,
    pub session_duration: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct Project {
    pub name: String,
    pub dir: String,
    pub workspace: String,
    pub workflow: String,
    pub opentofu: bool,
    pub include_patterns: Vec<String>,
    pub exclude_patterns: Vec<String>,
    pub depends_on: Vec<String>,
    pub layer: u32,
    pub alias: Option<String>,
    pub block_name: Option<String>,
    pub branch: Option<String>,
    pub apply_requirements: Vec<String>,
    pub drift_detection: bool,
    pub drift_include_patterns: Vec<String>,
    pub drift_exclude_patterns: Vec<String>,
    pub aws_role_to_assume: Option<AwsRoleToAssume>,
    pub aws_cognito_oidc: Option<AwsCognitoOidc>,
    pub generated: bool,
}

impl Project {
    pub fn display_name(&self) -> &str {
        self.alias.as_deref().unwrap_or(&self.name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Init {
        extra_args: Vec<String>,
    },
    Plan {
        extra_args: Vec<String>,
    },
    Apply {
        extra_args: Vec<String>,
    },
    Run {
        command: String,
        shell: Option<String>,
    },
}

impl Step {
    pub fn is_init(&self) -> bool {
        matches!(self, Step::Init { .. })
    }
    pub fn name(&self) -> &'static str {
        match self {
            Step::Init { .. } => "init",
            Step::Plan { .. } => "plan",
            Step::Apply { .. } => "apply",
            Step::Run { .. } => "run",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stage {
    pub filter_regex: Option<String>,
    pub steps: Vec<Step>,
}

impl Stage {
    pub fn default_plan() -> Self {
        Stage {
            filter_regex: None,
            steps: vec![
                Step::Init { extra_args: vec![] },
                Step::Plan { extra_args: vec![] },
            ],
        }
    }
    pub fn default_apply() -> Self {
        Stage {
            filter_regex: None,
            steps: vec![
                Step::Init { extra_args: vec![] },
                Step::Apply { extra_args: vec![] },
            ],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvVar {
    pub name: String,
    pub value: Option<String>,
    pub value_from: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvVars {
    pub state: Vec<EnvVar>,
    pub commands: Vec<EnvVar>,
}

impl EnvVars {
    /// Resolve `value` / `value_from` into concrete maps, reading
    /// `value_from` from the process environment.
    pub fn resolve(&self) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
        fn resolve_list(list: &[EnvVar]) -> BTreeMap<String, String> {
            let mut out = BTreeMap::new();
            for v in list {
                let value = if let Some(val) = &v.value {
                    val.clone()
                } else if let Some(from) = &v.value_from {
                    std::env::var(from).unwrap_or_default()
                } else {
                    String::new()
                };
                out.insert(v.name.clone(), value);
            }
            out
        }
        (resolve_list(&self.state), resolve_list(&self.commands))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowConfiguration {
    pub on_pull_request_pushed: Vec<String>,
    pub on_pull_request_closed: Vec<String>,
    pub on_commit_to_default: Vec<String>,
    pub on_pull_request_to_draft: Vec<String>,
    pub skip_merge_check: bool,
}

impl Default for WorkflowConfiguration {
    fn default() -> Self {
        Self {
            on_pull_request_pushed: vec!["digger plan".to_string()],
            on_pull_request_closed: vec!["digger unlock".to_string()],
            on_commit_to_default: vec!["digger unlock".to_string()],
            on_pull_request_to_draft: vec![],
            skip_merge_check: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workflow {
    pub env_vars: EnvVars,
    pub plan: Stage,
    pub apply: Stage,
    pub configuration: WorkflowConfiguration,
}

impl Default for Workflow {
    fn default() -> Self {
        Self {
            env_vars: EnvVars::default(),
            plan: Stage::default_plan(),
            apply: Stage::default_apply(),
            configuration: WorkflowConfiguration::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub projects: Vec<Project>,
    pub workflows: BTreeMap<String, Workflow>,
    pub dependency_mode: DependencyMode,
    pub traverse_to_nested_projects: bool,
    pub auto_merge: bool,
    pub auto_merge_strategy: MergeStrategy,
    pub pr_locks: bool,
    pub allow_draft_prs: bool,
    pub drift_include_patterns: Vec<String>,
    pub drift_exclude_patterns: Vec<String>,
    pub concurrency: Concurrency,
    pub comment: CommentConfig,
    pub graph: DependencyGraph,
}

impl Config {
    pub fn project(&self, name: &str) -> Option<&Project> {
        self.projects.iter().find(|p| p.name == name)
    }

    pub fn workflow_for(&self, project: &Project) -> &Workflow {
        self.workflows
            .get(&project.workflow)
            .expect("validated: workflow exists")
    }
}

/// Where the configuration came from.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub config: Config,
    pub path: PathBuf,
    pub legacy_name: bool,
}

/// Locate the configuration file following the search order in DESIGN.md §5.1.
pub fn find_config_file(
    dir: &Path,
    explicit: Option<&Path>,
) -> Result<(PathBuf, bool), ConfigError> {
    if let Some(p) = explicit {
        let p = if p.is_absolute() {
            p.to_path_buf()
        } else {
            dir.join(p)
        };
        if !p.exists() {
            return Err(ConfigError::NotFound(p.display().to_string()));
        }
        return Ok((p, false));
    }
    let existing = |names: &[&str]| -> Vec<PathBuf> {
        names
            .iter()
            .map(|n| dir.join(n))
            .filter(|p| p.is_file())
            .collect()
    };
    let primary = existing(&CONFIG_FILE_NAMES);
    if primary.len() > 1 {
        return Err(ConfigError::Conflict(
            primary[0].display().to_string(),
            primary[1].display().to_string(),
        ));
    }
    if let Some(p) = primary.into_iter().next() {
        return Ok((p, false));
    }
    let legacy = existing(&LEGACY_CONFIG_FILE_NAMES);
    if legacy.len() > 1 {
        return Err(ConfigError::Conflict(
            legacy[0].display().to_string(),
            legacy[1].display().to_string(),
        ));
    }
    if let Some(p) = legacy.into_iter().next() {
        return Ok((p, true));
    }
    Err(ConfigError::NotFound(
        CONFIG_FILE_NAMES
            .iter()
            .chain(LEGACY_CONFIG_FILE_NAMES.iter())
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
            .join(", "),
    ))
}

/// Load, expand `generate_projects`, validate and return the configuration.
pub fn load(dir: &Path, explicit: Option<&Path>) -> Result<Loaded, ConfigError> {
    let (path, legacy_name) = find_config_file(dir, explicit)?;
    let text = std::fs::read_to_string(&path).map_err(|e| ConfigError::Io {
        path: path.clone(),
        source: e,
    })?;
    let config = load_from_str(&text, dir).map_err(|e| match e {
        ConfigError::Yaml { source, .. } => ConfigError::Yaml {
            path: path.clone(),
            source,
        },
        other => other,
    })?;
    Ok(Loaded {
        config,
        path,
        legacy_name,
    })
}

/// Parse configuration text. `dir` is the repository root used for
/// `generate_projects` directory discovery.
pub fn load_from_str(text: &str, dir: &Path) -> Result<Config, ConfigError> {
    let raw: raw::RawConfig = serde_yaml::from_str(text).map_err(|e| ConfigError::Yaml {
        path: PathBuf::from("<inline>"),
        source: e,
    })?;
    let unsupported = raw.unsupported_keys();
    if !unsupported.is_empty() {
        return Err(ConfigError::Unsupported(unsupported));
    }
    convert::convert(raw, dir)
}
