//! Terraform / OpenTofu process runner.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IacTool {
    Terraform,
    OpenTofu,
}

impl IacTool {
    pub fn binary(&self) -> &'static str {
        match self {
            IacTool::Terraform => "terraform",
            IacTool::OpenTofu => "tofu",
        }
    }
    pub fn display(&self) -> &'static str {
        match self {
            IacTool::Terraform => "Terraform",
            IacTool::OpenTofu => "OpenTofu",
        }
    }
}

/// Result of one subprocess invocation.
#[derive(Debug, Clone, Default)]
pub struct CmdOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl CmdOutput {
    pub fn success(&self) -> bool {
        self.exit_code == 0
    }
}

/// Runs `terraform` / `tofu` in a working directory.
#[derive(Debug, Clone)]
pub struct IacRunner {
    pub tool: IacTool,
    pub working_dir: PathBuf,
    pub workspace: String,
    /// Prefix for console output lines, e.g. `[my-project]`.
    pub log_prefix: String,
    /// Regex whose matches are replaced with `<REDACTED>` in captured and
    /// printed output.
    pub filter: Option<regex::Regex>,
    /// Extra environment applied to every invocation (e.g. `TF_DATA_DIR`).
    pub base_env: BTreeMap<String, String>,
}

const LOCK_TIMEOUT: &str = "-lock-timeout=3m";

pub(crate) fn init_args(extra_args: &[String]) -> Vec<String> {
    let mut args = vec!["init".to_string()];
    args.extend(expand_args(extra_args));
    args.push("-input=false".into());
    args.push("-no-color".into());
    args
}

pub(crate) fn plan_args(extra_args: &[String], plan_path: &Path) -> Vec<String> {
    let mut args = vec!["plan".to_string()];
    args.extend(expand_args(extra_args));
    args.extend([
        "-input=false".to_string(),
        "-no-color".to_string(),
        "-detailed-exitcode".to_string(),
        LOCK_TIMEOUT.to_string(),
        "-out".to_string(),
        plan_path.to_string_lossy().to_string(),
    ]);
    args
}

pub(crate) fn apply_args(extra_args: &[String], plan_path: Option<&Path>) -> Vec<String> {
    let mut args = vec!["apply".to_string()];
    args.extend(expand_args(extra_args));
    args.extend([
        LOCK_TIMEOUT.to_string(),
        "-input=false".to_string(),
        "-no-color".to_string(),
        "-auto-approve".to_string(),
    ]);
    if let Some(p) = plan_path {
        args.push(p.to_string_lossy().to_string());
    }
    args
}

pub(crate) fn destroy_args(extra_args: &[String]) -> Vec<String> {
    let mut args = vec!["destroy".to_string()];
    args.extend(expand_args(extra_args));
    args.extend([
        LOCK_TIMEOUT.to_string(),
        "-input=false".to_string(),
        "-no-color".to_string(),
        "-auto-approve".to_string(),
    ]);
    args
}

impl IacRunner {
    pub async fn init(
        &self,
        extra_args: &[String],
        env: &BTreeMap<String, String>,
    ) -> std::io::Result<CmdOutput> {
        let args = init_args(extra_args);
        let out = self.run(&args, env, true).await?;
        if !out.success() {
            return Ok(out);
        }
        if self.workspace != "default" && !self.workspace.is_empty() {
            let select = self
                .run(
                    &[
                        "workspace".into(),
                        "select".into(),
                        "-no-color".into(),
                        self.workspace.clone(),
                    ],
                    env,
                    true,
                )
                .await?;
            if !select.success() {
                let new = self
                    .run(
                        &[
                            "workspace".into(),
                            "new".into(),
                            "-no-color".into(),
                            self.workspace.clone(),
                        ],
                        env,
                        true,
                    )
                    .await?;
                if !new.success() {
                    return Ok(new);
                }
            }
        }
        Ok(out)
    }

    /// Returns the command output; exit code 2 means "changes present".
    pub async fn plan(
        &self,
        extra_args: &[String],
        env: &BTreeMap<String, String>,
        plan_path: &Path,
    ) -> std::io::Result<CmdOutput> {
        self.run(&plan_args(extra_args, plan_path), env, true).await
    }

    pub async fn apply(
        &self,
        extra_args: &[String],
        env: &BTreeMap<String, String>,
        plan_path: Option<&Path>,
    ) -> std::io::Result<CmdOutput> {
        self.run(&apply_args(extra_args, plan_path), env, true)
            .await
    }

    pub async fn destroy(
        &self,
        extra_args: &[String],
        env: &BTreeMap<String, String>,
    ) -> std::io::Result<CmdOutput> {
        self.run(&destroy_args(extra_args), env, true).await
    }

    pub async fn show_json(
        &self,
        env: &BTreeMap<String, String>,
        plan_path: &Path,
    ) -> std::io::Result<CmdOutput> {
        let args = vec![
            "show".to_string(),
            "-no-color".to_string(),
            "-json".to_string(),
            plan_path.to_string_lossy().to_string(),
        ];
        self.run(&args, env, false).await
    }

    pub async fn show_text(
        &self,
        env: &BTreeMap<String, String>,
        plan_path: &Path,
    ) -> std::io::Result<CmdOutput> {
        let args = vec![
            "show".to_string(),
            "-no-color".to_string(),
            plan_path.to_string_lossy().to_string(),
        ];
        self.run(&args, env, false).await
    }

    async fn run(
        &self,
        args: &[String],
        env: &BTreeMap<String, String>,
        print: bool,
    ) -> std::io::Result<CmdOutput> {
        tracing::info!(
            target: "nikama::exec",
            "{} $ {} {}",
            self.log_prefix,
            self.tool.binary(),
            redact_args(args).join(" ")
        );
        let mut cmd = tokio::process::Command::new(self.tool.binary());
        cmd.args(args)
            .current_dir(&self.working_dir)
            .env("TF_IN_AUTOMATION", "true")
            .envs(&self.base_env)
            .envs(env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        run_streaming(cmd, &self.log_prefix, self.filter.clone(), print).await
    }
}

/// Spawn a process and stream its output to the console (prefixed) while
/// capturing it.
pub async fn run_streaming(
    mut cmd: tokio::process::Command,
    prefix: &str,
    filter: Option<regex::Regex>,
    print: bool,
) -> std::io::Result<CmdOutput> {
    let mut child = cmd.spawn()?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let p1 = prefix.to_string();
    let p2 = prefix.to_string();
    let f1 = filter.clone();
    let f2 = filter;
    let out_task = tokio::spawn(async move { collect(stdout, &p1, f1, print, false).await });
    let err_task = tokio::spawn(async move { collect(stderr, &p2, f2, print, true).await });
    let status = child.wait().await?;
    let stdout = out_task.await.unwrap_or_default();
    let stderr = err_task.await.unwrap_or_default();
    Ok(CmdOutput {
        exit_code: status.code().unwrap_or(-1),
        stdout,
        stderr,
    })
}

async fn collect<R: tokio::io::AsyncRead + Unpin>(
    reader: R,
    prefix: &str,
    filter: Option<regex::Regex>,
    print: bool,
    is_err: bool,
) -> String {
    let mut lines = BufReader::new(reader).lines();
    let mut buf = String::new();
    while let Ok(Some(line)) = lines.next_line().await {
        let line = match &filter {
            Some(re) => re.replace_all(&line, "<REDACTED>").into_owned(),
            None => line,
        };
        if print {
            if is_err {
                eprintln!("{} {}", prefix, line);
            } else {
                println!("{} {}", prefix, line);
            }
        }
        buf.push_str(&line);
        buf.push('\n');
    }
    buf
}

/// Expand `$VAR` in arguments and drop empty ones.
pub fn expand_args(args: &[String]) -> Vec<String> {
    args.iter()
        .map(|a| crate::util::expand_env(a).trim().to_string())
        .filter(|a| !a.is_empty())
        .collect()
}

/// Mask secrets passed via `-backend-config=` for logging.
pub fn redact_args(args: &[String]) -> Vec<String> {
    args.iter()
        .map(|a| {
            for key in ["access_key=", "secret_key=", "token=", "password="] {
                if let Some(pos) = a.find(key) {
                    let (head, _) = a.split_at(pos + key.len());
                    return format!("{}<REDACTED>", head);
                }
            }
            a.clone()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts() {
        let v = redact_args(&["-backend-config=secret_key=abc".into(), "-upgrade".into()]);
        assert_eq!(v, vec!["-backend-config=secret_key=<REDACTED>", "-upgrade"]);
    }
}
