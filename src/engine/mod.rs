//! Job model and execution engine (Terraform / OpenTofu).

mod exec;
mod output;
mod plan_json;
mod runner;

pub use exec::{ApplyResult, ExecError, JobExecutor, PlanResult};
pub use plan_json::{plan_summary_table, PlanSummary};
pub use runner::IacTool;

use crate::config::{Project, Workflow};
use std::collections::BTreeMap;

/// A PR / manual command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Command {
    Plan,
    Apply,
    Destroy,
    Lock,
    Unlock,
}

impl Command {
    /// Accepts `digger plan`, `nikama plan` and bare `plan` (case-insensitive).
    pub fn parse(s: &str) -> Option<Command> {
        let s = s.trim().to_ascii_lowercase();
        let s = s
            .strip_prefix("digger ")
            .or_else(|| s.strip_prefix("nikama "))
            .unwrap_or(&s)
            .trim();
        match s {
            "plan" => Some(Command::Plan),
            "apply" => Some(Command::Apply),
            "destroy" => Some(Command::Destroy),
            "lock" => Some(Command::Lock),
            "unlock" => Some(Command::Unlock),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Command::Plan => "plan",
            Command::Apply => "apply",
            Command::Destroy => "destroy",
            Command::Lock => "lock",
            Command::Unlock => "unlock",
        }
    }

    /// Commit status context used for this command.
    pub fn status_context(&self) -> &'static str {
        match self {
            Command::Apply | Command::Destroy => "digger/apply",
            _ => "digger/plan",
        }
    }
}

impl std::fmt::Display for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One unit of work: a command against one project.
#[derive(Debug, Clone)]
pub struct Job {
    pub project: Project,
    pub workflow: Workflow,
    pub command: Command,
    pub pr_number: Option<u64>,
    pub event_name: String,
    pub requested_by: String,
    /// `owner/repo`
    pub namespace: String,
    pub run_env: BTreeMap<String, String>,
    pub state_env: BTreeMap<String, String>,
    pub command_env: BTreeMap<String, String>,
    /// Extra `-backend-config=...` arguments injected into `init` by the AWS
    /// "backend-config" credential strategy.
    pub backend_config_args: Vec<String>,
    pub skip_merge_check: bool,
}

impl Job {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        project: &Project,
        workflow: &Workflow,
        command: Command,
        pr_number: Option<u64>,
        event_name: &str,
        requested_by: &str,
        namespace: &str,
        pr_branch: &str,
        default_branch: &str,
    ) -> Job {
        let (state_env, command_env) = workflow.env_vars.resolve();
        let mut run_env = BTreeMap::new();
        run_env.insert("DEFAULT_BRANCH".to_string(), default_branch.to_string());
        run_env.insert("PR_BRANCH".to_string(), pr_branch.to_string());
        run_env.insert("PROJECT_NAME".to_string(), project.name.clone());
        run_env.insert("PROJECT_DIR".to_string(), project.dir.clone());
        let skip_merge_check = workflow.configuration.skip_merge_check
            || !project.apply_requirements.iter().any(|r| r == "mergeable");
        Job {
            project: project.clone(),
            workflow: workflow.clone(),
            command,
            pr_number,
            event_name: event_name.to_string(),
            requested_by: requested_by.to_string(),
            namespace: namespace.to_string(),
            run_env,
            state_env,
            command_env,
            backend_config_args: vec![],
            skip_merge_check,
        }
    }

    pub fn tool(&self) -> IacTool {
        if self.project.opentofu {
            IacTool::OpenTofu
        } else {
            IacTool::Terraform
        }
    }

    /// `owner/repo#project` — the PR lock resource id.
    pub fn lock_id(&self) -> String {
        format!("{}#{}", self.namespace, self.project.name)
    }

    /// `<owner>-<repo>[-<pr>]-<project>.tfplan` — the stored plan key.
    pub fn plan_file_name(&self) -> String {
        let ns = self.namespace.replace('/', "-");
        match self.pr_number {
            Some(pr) => format!("{}-{}-{}.tfplan", ns, pr, self.project.name),
            None => format!("{}-{}.tfplan", ns, self.project.name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands() {
        assert_eq!(Command::parse("digger plan"), Some(Command::Plan));
        assert_eq!(Command::parse("Nikama Apply"), Some(Command::Apply));
        assert_eq!(Command::parse("unlock"), Some(Command::Unlock));
        assert_eq!(Command::parse("digger noop"), None);
    }
}
