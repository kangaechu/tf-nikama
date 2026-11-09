//! Manual mode: run one command against one project (`workflow_dispatch`).

use super::exit;
use super::run::{run_job, Outcome, RunContext};
use crate::cli::Env;
use crate::config::Config;
use crate::engine::{plan_summary_table, Command, Job};
use crate::storage::NoopPlanStorage;
use std::sync::Arc;

pub async fn run(env: &Env, config: Arc<Config>, actor: &str, namespace: &str) -> i32 {
    let command_str = match crate::util::env_first(&["INPUT_DIGGER_COMMAND", "NIKAMA_COMMAND"]) {
        Some(c) => c,
        None => {
            tracing::error!(
                "manual mode requires INPUT_DIGGER_COMMAND (digger plan|apply|destroy)"
            );
            return exit::CONFIG;
        }
    };
    let command = match Command::parse(&command_str) {
        Some(c @ (Command::Plan | Command::Apply | Command::Destroy)) => c,
        _ => {
            tracing::error!(command = %command_str, "manual mode supports plan, apply and destroy only");
            return exit::COMMAND;
        }
    };
    let project_name = match crate::util::env_first(&["INPUT_DIGGER_PROJECT", "NIKAMA_PROJECT"]) {
        Some(p) => p,
        None => {
            tracing::error!("manual mode requires INPUT_DIGGER_PROJECT");
            return exit::CONFIG;
        }
    };
    let Some(project) = config.project(&project_name) else {
        tracing::error!(
            project = %project_name,
            available = %config.projects.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", "),
            "project not found"
        );
        return exit::CONFIG;
    };
    let workflow = config.workflow_for(project);
    let mut job = Job::new(
        project,
        workflow,
        command,
        None,
        "manual_invocation",
        actor,
        namespace,
        "",
        "",
    );
    job.skip_merge_check = true;

    let ctx = Arc::new(RunContext {
        config: config.clone(),
        repo_root: env.repo_root.clone(),
        work_root: env.work_root.clone(),
        init_lock: env.init_lock(),
        storage: Arc::new(NoopPlanStorage),
        locker: None,
        writer: None,
        max_parallel: 1,
    });
    let result = run_job(&ctx, job).await;
    match result.outcome {
        Ok(Outcome::Plan(p)) => {
            println!(
                "\n==== plan: {} ({}) ====\n{}\n",
                project_name,
                p.summary.short(),
                p.plan_text
            );
            let table = plan_summary_table(&p.summary);
            if !table.is_empty() {
                println!("{}", table);
            }
            exit::OK
        }
        Ok(Outcome::Apply(a)) | Ok(Outcome::Destroy(a)) => {
            println!("\n==== {}: {} ====\n{}\n", command, project_name, a.output);
            exit::OK
        }
        Ok(other) => {
            tracing::info!(?other, "done");
            exit::OK
        }
        Err(e) => {
            tracing::error!(project = %project_name, error = %e, "command failed");
            exit::JOB_FAILED
        }
    }
}
