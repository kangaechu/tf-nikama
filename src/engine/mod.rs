//! Job model and execution engine (Terraform / OpenTofu).

mod exec;
#[cfg(test)]
mod exec_tests;
mod output;
mod plan_json;
pub mod providers;
mod runner;

pub use exec::{ApplyResult, ExecError, JobExecutor, PlanResult};
pub use plan_json::{plan_summary_table, ApplySummary, PlanSummary};
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
        Self::parse_with(s, &[])
    }

    /// Like [`Command::parse`], additionally accepting the configured wakeup
    /// words as prefix. The default words stay accepted so that
    /// `workflow_configuration` defaults (`nikama plan`, ...) and Digger-era
    /// entries (`digger plan`) keep working whatever `wakeup_word` says.
    pub fn parse_with(s: &str, wakeup_words: &[String]) -> Option<Command> {
        let s = s.trim().to_ascii_lowercase();
        let s = crate::config::DEFAULT_WAKEUP_WORDS
            .iter()
            .map(|w| w.to_ascii_lowercase())
            .chain(wakeup_words.iter().map(|w| w.to_ascii_lowercase()))
            .find_map(|w| s.strip_prefix(&format!("{} ", w)))
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
    /// Arguments from a PR comment (`-- -target=...`) appended to `plan`
    /// as given, without environment expansion.
    pub extra_args: Vec<String>,
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
        // Only the explicit workflow flag skips the mergeable check; the
        // project's `apply_requirements` are evaluated one by one at apply time.
        let skip_merge_check = workflow.configuration.skip_merge_check;
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
            extra_args: vec![],
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
        plan_file_name(&self.namespace, self.pr_number, &self.project.name)
    }
}

/// Stored plan key for a project (see [`Job::plan_file_name`]).
pub fn plan_file_name(namespace: &str, pr_number: Option<u64>, project: &str) -> String {
    let ns = namespace.replace('/', "-");
    match pr_number {
        Some(pr) => format!("{}-{}-{}.tfplan", ns, pr, project),
        None => format!("{}-{}.tfplan", ns, project),
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

    #[test]
    fn parses_commands_with_configured_wakeup_word() {
        let words = vec!["tf".to_string()];
        assert_eq!(Command::parse_with("tf plan", &words), Some(Command::Plan));
        assert_eq!(
            Command::parse_with("TF Apply", &words),
            Some(Command::Apply)
        );
        // The defaults keep working for workflow_configuration entries.
        assert_eq!(
            Command::parse_with("digger unlock", &words),
            Some(Command::Unlock)
        );
        assert_eq!(Command::parse_with("tf", &words), None);
        assert_eq!(Command::parse("tf plan"), None);
    }
}
