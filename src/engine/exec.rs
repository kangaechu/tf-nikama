//! Executes a job's plan / apply / destroy stages.

use super::output::{cleanup_apply_output, cleanup_plan_output};
use super::plan_json::{apply_summary_from_output, plan_summary_from_json, PlanSummary};
use super::runner::{run_streaming, CmdOutput, IacRunner};
use super::Job;
use crate::config::Step;
use crate::util::merge_maps;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone)]
pub struct StepLog {
    pub step: String,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone)]
pub struct ExecError {
    pub step: String,
    pub message: String,
    pub stdout: String,
    pub stderr: String,
}

impl std::fmt::Display for ExecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} failed: {}", self.step, self.message)
    }
}

impl std::error::Error for ExecError {}

#[derive(Debug, Clone)]
pub struct PlanResult {
    pub summary: PlanSummary,
    pub plan_text: String,
    pub plan_json: String,
    pub plan_file: PathBuf,
    pub additional_output: Option<String>,
    pub steps: Vec<StepLog>,
}

impl PlanResult {
    pub fn is_empty(&self) -> bool {
        self.summary.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct ApplyResult {
    pub output: String,
    /// `(added, changed, destroyed)` when parseable.
    pub summary: Option<(u32, u32, u32)>,
    pub additional_output: Option<String>,
    pub steps: Vec<StepLog>,
}

/// Runs the stages of one job.
pub struct JobExecutor<'a> {
    pub job: &'a Job,
    /// Repository root (where `nikama.yml` lives).
    pub repo_root: PathBuf,
    /// Per-job scratch directory for plan files and additional output.
    pub work_dir: PathBuf,
    /// When set, `init` steps are serialised through this mutex (Terraform's
    /// plugin cache is not concurrency safe).
    pub init_lock: Option<Arc<Mutex<()>>>,
    /// Use a per-job `TF_DATA_DIR` (needed when several projects share a dir).
    pub isolate_data_dir: bool,
}

impl<'a> JobExecutor<'a> {
    pub fn project_path(&self) -> PathBuf {
        self.repo_root.join(&self.job.project.dir)
    }

    pub fn plan_file(&self) -> PathBuf {
        self.work_dir.join(self.job.plan_file_name())
    }

    fn additional_output_file(&self) -> PathBuf {
        self.work_dir.join("additional-output.log")
    }

    fn runner(&self, filter_regex: Option<&str>) -> IacRunner {
        let filter = filter_regex.and_then(|r| regex::Regex::new(r).ok());
        let mut base_env = BTreeMap::new();
        if self.isolate_data_dir {
            base_env.insert(
                "TF_DATA_DIR".to_string(),
                self.work_dir
                    .join(".terraform")
                    .to_string_lossy()
                    .to_string(),
            );
        }
        IacRunner {
            tool: self.job.tool(),
            working_dir: self.project_path(),
            workspace: self.job.project.workspace.clone(),
            log_prefix: format!("[{}]", self.job.project.name),
            filter,
            base_env,
        }
    }

    fn run_env(&self, plan_file: Option<&Path>) -> BTreeMap<String, String> {
        let mut env = self.job.run_env.clone();
        if let Some(p) = plan_file {
            env.insert(
                "DIGGER_PLANFILE".to_string(),
                p.to_string_lossy().to_string(),
            );
            env.insert(
                "NIKAMA_PLANFILE".to_string(),
                p.to_string_lossy().to_string(),
            );
        }
        let out = self.additional_output_file().to_string_lossy().to_string();
        env.insert("DIGGER_OUT".to_string(), out.clone());
        env.insert("NIKAMA_OUT".to_string(), out);
        env
    }

    async fn init(
        &self,
        runner: &IacRunner,
        extra_args: &[String],
    ) -> Result<CmdOutput, ExecError> {
        let mut args: Vec<String> = extra_args.to_vec();
        args.extend(self.job.backend_config_args.iter().cloned());
        let _guard = match &self.init_lock {
            Some(m) => Some(m.lock().await),
            None => None,
        };
        let out = runner
            .init(&args, &self.job.state_env)
            .await
            .map_err(|e| io_err("init", e))?;
        if !out.success() {
            return Err(ExecError {
                step: "init".into(),
                message: format!("exit code {}", out.exit_code),
                stdout: out.stdout,
                stderr: out.stderr,
            });
        }
        Ok(out)
    }

    async fn run_shell(
        &self,
        command: &str,
        shell: Option<&str>,
        plan_file: Option<&Path>,
    ) -> Result<CmdOutput, ExecError> {
        let script = self.work_dir.join(format!("run-step-{}.sh", uuid_like()));
        std::fs::write(&script, format!("{}\n", command)).map_err(|e| io_err("run", e))?;
        let mut cmd = match shell {
            Some(sh) if !sh.trim().is_empty() => {
                let mut parts = shell_words::split(sh).unwrap_or_else(|_| vec![sh.to_string()]);
                let bin = parts.remove(0);
                let mut c = tokio::process::Command::new(bin);
                c.args(parts);
                c
            }
            _ => {
                let mut c = tokio::process::Command::new("bash");
                c.arg("-eo").arg("pipefail");
                c
            }
        };
        cmd.arg(&script)
            .current_dir(self.project_path())
            .envs(self.run_env(plan_file))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        tracing::info!(target: "nikama::exec", "[{}] $ {}", self.job.project.name, command);
        let out = run_streaming(cmd, &format!("[{}]", self.job.project.name), None, true)
            .await
            .map_err(|e| io_err("run", e))?;
        let _ = std::fs::remove_file(&script);
        if !out.success() {
            return Err(ExecError {
                step: "run".into(),
                message: format!("'{}' exited with code {}", command, out.exit_code),
                stdout: out.stdout,
                stderr: out.stderr,
            });
        }
        Ok(out)
    }

    fn take_additional_output(&self) -> Option<String> {
        let p = self.additional_output_file();
        let content = std::fs::read_to_string(&p).ok()?;
        let _ = std::fs::remove_file(&p);
        let trimmed = content.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    /// Run the plan stage. Returns the plan even when it is empty.
    pub async fn plan(&self) -> Result<PlanResult, ExecError> {
        std::fs::create_dir_all(&self.work_dir).map_err(|e| io_err("plan", e))?;
        let stage = &self.job.workflow.plan;
        let runner = self.runner(stage.filter_regex.as_deref());
        let plan_file = self.plan_file();
        let mut steps = Vec::new();
        let mut plan_stdout: Option<String> = None;
        let env = merge_maps(&self.job.command_env, &BTreeMap::new());

        for step in &stage.steps {
            match step {
                Step::Init { extra_args } => {
                    let out = self.init(&runner, extra_args).await?;
                    steps.push(log("init", &out));
                }
                Step::Plan { extra_args } => {
                    let out = runner
                        .plan(extra_args, &env, &plan_file)
                        .await
                        .map_err(|e| io_err("plan", e))?;
                    steps.push(log("plan", &out));
                    if out.exit_code != 0 && out.exit_code != 2 {
                        return Err(ExecError {
                            step: "plan".into(),
                            message: format!("exit code {}", out.exit_code),
                            stdout: out.stdout,
                            stderr: out.stderr,
                        });
                    }
                    plan_stdout = Some(out.stdout);
                }
                Step::Apply { .. } => {
                    tracing::warn!("ignoring 'apply' step inside plan stage");
                }
                Step::Run { command, shell } => {
                    let pf = if plan_file.exists() {
                        Some(plan_file.as_path())
                    } else {
                        None
                    };
                    let out = self.run_shell(command, shell.as_deref(), pf).await?;
                    steps.push(log("run", &out));
                }
            }
        }

        if !plan_file.exists() {
            return Err(ExecError {
                step: "plan".into(),
                message: format!(
                    "no plan file was produced at {} (does the plan stage contain a 'plan' step?)",
                    plan_file.display()
                ),
                stdout: String::new(),
                stderr: String::new(),
            });
        }

        let show = runner
            .show_json(&env, &plan_file)
            .await
            .map_err(|e| io_err("show", e))?;
        if !show.success() {
            return Err(ExecError {
                step: "show".into(),
                message: format!("exit code {}", show.exit_code),
                stdout: show.stdout,
                stderr: show.stderr,
            });
        }
        let summary = plan_summary_from_json(&show.stdout).map_err(|e| ExecError {
            step: "show".into(),
            message: format!("could not parse plan JSON: {}", e),
            stdout: String::new(),
            stderr: String::new(),
        })?;

        let plan_text = match plan_stdout {
            Some(s) => cleanup_plan_output(&s),
            None => {
                // Custom plan step wrote the file itself; render it.
                let t = runner
                    .show_text(&env, &plan_file)
                    .await
                    .map_err(|e| io_err("show", e))?;
                cleanup_plan_output(&t.stdout)
            }
        };

        Ok(PlanResult {
            summary,
            plan_text,
            plan_json: show.stdout,
            plan_file,
            additional_output: self.take_additional_output(),
            steps,
        })
    }

    /// Run the apply stage. `plan_file` is used when present.
    pub async fn apply(&self, plan_file: Option<&Path>) -> Result<ApplyResult, ExecError> {
        std::fs::create_dir_all(&self.work_dir).map_err(|e| io_err("apply", e))?;
        let stage = &self.job.workflow.apply;
        let runner = self.runner(stage.filter_regex.as_deref());
        let mut steps = Vec::new();
        let mut output = String::new();
        let mut summary = None;
        for step in &stage.steps {
            match step {
                Step::Init { extra_args } => {
                    let out = self.init(&runner, extra_args).await?;
                    steps.push(log("init", &out));
                }
                Step::Apply { extra_args } => {
                    let out = runner
                        .apply(extra_args, &self.job.command_env, plan_file)
                        .await
                        .map_err(|e| io_err("apply", e))?;
                    steps.push(log("apply", &out));
                    if !out.success() {
                        return Err(ExecError {
                            step: "apply".into(),
                            message: format!("exit code {}", out.exit_code),
                            stdout: cleanup_apply_output(&out.stdout),
                            stderr: out.stderr,
                        });
                    }
                    output = cleanup_apply_output(&out.stdout);
                    summary = apply_summary_from_output(&out.stdout);
                }
                Step::Plan { .. } => {
                    tracing::warn!("ignoring 'plan' step inside apply stage");
                }
                Step::Run { command, shell } => {
                    let out = self.run_shell(command, shell.as_deref(), plan_file).await?;
                    steps.push(log("run", &out));
                }
            }
        }
        Ok(ApplyResult {
            output,
            summary,
            additional_output: self.take_additional_output(),
            steps,
        })
    }

    /// `init` then `destroy` (stages are not configurable for destroy).
    pub async fn destroy(&self) -> Result<ApplyResult, ExecError> {
        std::fs::create_dir_all(&self.work_dir).map_err(|e| io_err("destroy", e))?;
        let runner = self.runner(self.job.workflow.apply.filter_regex.as_deref());
        let init_args = match self.job.workflow.apply.steps.first() {
            Some(Step::Init { extra_args }) => extra_args.clone(),
            _ => vec![],
        };
        let mut steps = Vec::new();
        let out = self.init(&runner, &init_args).await?;
        steps.push(log("init", &out));
        let out = runner
            .destroy(&[], &self.job.command_env)
            .await
            .map_err(|e| io_err("destroy", e))?;
        steps.push(log("destroy", &out));
        if !out.success() {
            return Err(ExecError {
                step: "destroy".into(),
                message: format!("exit code {}", out.exit_code),
                stdout: cleanup_apply_output(&out.stdout),
                stderr: out.stderr,
            });
        }
        Ok(ApplyResult {
            summary: apply_summary_from_output(&out.stdout),
            output: cleanup_apply_output(&out.stdout),
            additional_output: self.take_additional_output(),
            steps,
        })
    }
}

fn log(step: &str, out: &CmdOutput) -> StepLog {
    StepLog {
        step: step.to_string(),
        exit_code: out.exit_code,
        stdout: out.stdout.clone(),
        stderr: out.stderr.clone(),
    }
}

fn io_err(step: &str, e: std::io::Error) -> ExecError {
    ExecError {
        step: step.to_string(),
        message: format!("failed to start process: {}", e),
        stdout: String::new(),
        stderr: String::new(),
    }
}

fn uuid_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}-{:x}", std::process::id(), nanos)
}
