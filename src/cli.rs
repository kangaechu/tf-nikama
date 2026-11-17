//! Command-line entry point: argument / environment handling and mode dispatch.

use crate::config::{self, Config};
use crate::github::{GithubClient, GithubContext};
use crate::modes::{self, exit};
use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Parser, Debug)]
#[command(
    name = "nikama",
    version,
    about = "Backendless Terraform/OpenTofu PR automation for GitHub Actions"
)]
pub struct Args {
    /// Path to nikama.yml (default: nikama.yml / nikama.yaml, then digger.yml)
    #[arg(long, env = "NIKAMA_CONFIG")]
    pub config: Option<PathBuf>,

    /// Maximum number of plans to run concurrently (overrides nikama.yml)
    #[arg(long, env = "NIKAMA_MAX_PARALLEL")]
    pub max_parallel: Option<usize>,

    /// Log level: ERROR, WARN, INFO, DEBUG (default INFO; DIGGER_LOG_LEVEL is a fallback)
    #[arg(long, env = "NIKAMA_LOG_LEVEL")]
    pub log_level: Option<String>,

    /// Repository root (default: current directory)
    #[arg(long, env = "NIKAMA_REPO_ROOT")]
    pub repo_root: Option<PathBuf>,

    /// Skip the GitHub Actions detection (for local runs of manual mode)
    #[arg(long, env = "NIKAMA_LOCAL")]
    pub local: bool,

    /// Only load and validate the configuration, then print the projects
    #[arg(long)]
    pub validate: bool,
}

/// Runtime environment shared by the modes.
pub struct Env {
    pub repo_root: PathBuf,
    pub work_root: PathBuf,
    pub max_parallel: Option<usize>,
    pub serialize_init: bool,
    /// Terraform CLI configuration enforcing the provider allowlist, if any.
    pub cli_config: Option<String>,
}

impl Env {
    /// Mutex used to serialise `terraform init` when a plugin cache is in use.
    pub fn init_lock(&self) -> Option<Arc<Mutex<()>>> {
        if self.serialize_init {
            Some(Arc::new(Mutex::new(())))
        } else {
            None
        }
    }
}

/// `--log-level` (clap already fills it from `NIKAMA_LOG_LEVEL`), then the
/// Digger-compatible `DIGGER_LOG_LEVEL`, then `INFO`.
fn init_logging(level: Option<&str>) {
    let level = match level
        .map(str::to_string)
        .or_else(|| crate::util::env_opt("DIGGER_LOG_LEVEL"))
        .unwrap_or_else(|| "INFO".to_string())
        .to_ascii_uppercase()
        .as_str()
    {
        "DEBUG" => tracing::Level::DEBUG,
        "WARN" | "WARNING" => tracing::Level::WARN,
        "ERROR" => tracing::Level::ERROR,
        _ => tracing::Level::INFO,
    };
    let filter = tracing_subscriber::EnvFilter::builder()
        .with_default_directive(level.into())
        .from_env_lossy()
        .add_directive("hyper=warn".parse().unwrap())
        .add_directive("aws_config=warn".parse().unwrap())
        .add_directive("aws_smithy_runtime=warn".parse().unwrap())
        .add_directive("aws_runtime=warn".parse().unwrap())
        .add_directive("reqwest=warn".parse().unwrap())
        .add_directive("google_cloud_auth=warn".parse().unwrap())
        .add_directive("rustls=warn".parse().unwrap());
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stdout)
        .init();
}

pub async fn run() -> i32 {
    let args = Args::parse();
    init_logging(args.log_level.as_deref());
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "nikama starting");

    let repo_root = args
        .repo_root
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));

    let mode = crate::util::env_first(&["NIKAMA_MODE", "INPUT_DIGGER_MODE"])
        .unwrap_or_default()
        .to_ascii_lowercase();
    let pr_mode = matches!(mode.as_str(), "" | "pr" | "event");

    // GitHub identity.
    let token =
        crate::util::env_first(&["NIKAMA_GITHUB_TOKEN", "DIGGER_GITHUB_TOKEN", "GITHUB_TOKEN"]);
    let repository = crate::util::env_opt("GITHUB_REPOSITORY");
    let actor = crate::util::env_opt("GITHUB_ACTOR").unwrap_or_else(|| "unknown".to_string());

    // Configuration: from a trusted git ref in PR mode when `config-ref` is
    // set (so the pull request cannot change what it is run with), otherwise
    // from the working tree.
    let config_ref = crate::util::env_opt("NIKAMA_CONFIG_REF");
    let mut loaded = match (&config_ref, pr_mode && !args.validate && !args.local) {
        (Some(wanted), true) => {
            let (Some(token), Some(repository)) = (&token, &repository) else {
                tracing::error!("GITHUB_TOKEN and GITHUB_REPOSITORY are required for config-ref");
                return exit::CONFIG;
            };
            let Some((owner, repo)) = crate::util::split_repo(repository) else {
                tracing::error!(%repository, "GITHUB_REPOSITORY must be owner/repo");
                return exit::CONFIG;
            };
            let github = GithubClient::new(token, owner, repo, github_api_base().as_deref());
            let git_ref = match resolve_config_ref(wanted, &github).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!(error = %e, "could not resolve config-ref");
                    return exit::CONFIG;
                }
            };
            tracing::info!(%git_ref, "reading the configuration from GitHub (config-ref)");
            match config::load_from_ref(
                &github,
                &git_ref,
                explicit_config_path(&args).as_deref(),
                &repo_root,
            )
            .await
            {
                Ok(l) => l,
                Err(e) => {
                    tracing::error!(error = %e, "configuration error");
                    return exit::CONFIG;
                }
            }
        }
        (Some(_), false) => {
            tracing::info!("config-ref only applies to pull request runs; reading the configuration from the working tree");
            match load_local_config(&args, &repo_root) {
                Ok(l) => l,
                Err(code) => return code,
            }
        }
        (None, _) => match load_local_config(&args, &repo_root) {
            Ok(l) => l,
            Err(code) => return code,
        },
    };
    // `run:` steps execute arbitrary commands from the configuration; let the
    // workflow forbid them when the configuration is not trusted.
    let allow_run_steps = crate::util::env_opt("NIKAMA_ALLOW_RUN_STEPS")
        .map(|v| !v.trim().eq_ignore_ascii_case("false"))
        .unwrap_or(true);
    if let Err(e) = loaded.config.check_run_steps(allow_run_steps) {
        tracing::error!(error = %e, path = %loaded.path.display(), "configuration error");
        return exit::CONFIG;
    }
    if loaded.legacy_name {
        tracing::warn!(
            path = %loaded.path.display(),
            "using legacy configuration file name; please rename it to nikama.yml"
        );
    }
    if let Err(e) = apply_reporting_strategy(&mut loaded.config) {
        tracing::error!(error = %e, "REPORTING_STRATEGY error");
        return exit::CONFIG;
    }
    let config = Arc::new(loaded.config);
    tracing::info!(
        path = %loaded.path.display(),
        projects = config.projects.len(),
        workflows = config.workflows.len(),
        "configuration loaded"
    );
    let cli_config = match provider_cli_config(&config) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "allowed-providers error");
            return exit::CONFIG;
        }
    };
    if args.validate {
        print_projects(&config);
        return exit::OK;
    }
    for name in [
        "NO_BACKEND",
        "DIGGER_TOKEN",
        "DIGGER_HOSTNAME",
        "DIGGER_ORGANISATION",
        "DIGGER_RUN_SPEC",
    ] {
        if crate::util::env_opt(name).is_some() {
            tracing::debug!(var = name, "ignoring Digger backend variable");
        }
    }

    // CI detection.
    if !args.local && crate::util::env_opt("GITHUB_ACTIONS").is_none() {
        tracing::error!("GITHUB_ACTIONS is not set: nikama runs inside GitHub Actions (use --local for manual runs)");
        return exit::NO_CI;
    }

    let work_root = crate::util::env_opt("RUNNER_TEMP")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("nikama");
    let serialize_init = crate::util::env_opt("TF_PLUGIN_CACHE_DIR").is_some()
        || crate::util::env_is_true("NIKAMA_SERIALIZE_INIT");
    if serialize_init {
        tracing::info!("TF_PLUGIN_CACHE_DIR is set: 'init' steps will run one at a time");
    }
    let env = Env {
        repo_root: repo_root.clone(),
        work_root,
        max_parallel: args.max_parallel,
        serialize_init,
        cli_config,
    };

    match mode.as_str() {
        "manual" => {
            let namespace = repository
                .clone()
                .unwrap_or_else(|| "local/repo".to_string());
            modes::manual::run(&env, config, &actor, &namespace).await
        }
        "drift-detection" | "drift" => {
            let (Some(token), Some(repository)) = (token, repository) else {
                tracing::error!("GITHUB_TOKEN and GITHUB_REPOSITORY are required");
                return exit::CONFIG;
            };
            let Some((owner, repo)) = crate::util::split_repo(&repository) else {
                tracing::error!(%repository, "GITHUB_REPOSITORY must be owner/repo");
                return exit::CONFIG;
            };
            let github = GithubClient::new(&token, owner, repo, github_api_base().as_deref());
            modes::drift::run(&env, config, &actor, &repository, github).await
        }
        "" | "pr" | "event" => {
            let (Some(token), Some(repository)) = (token, repository) else {
                tracing::error!("GITHUB_TOKEN and GITHUB_REPOSITORY are required");
                return exit::CONFIG;
            };
            let Some(context_json) = crate::util::env_opt("GITHUB_CONTEXT") else {
                tracing::error!("GITHUB_CONTEXT is not set; pass `GITHUB_CONTEXT: ${{{{ toJson(github) }}}}` in the step env");
                return exit::CONTEXT;
            };
            let ctx = match GithubContext::parse(&context_json) {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!(error = %e, "could not parse GITHUB_CONTEXT");
                    return exit::CONTEXT;
                }
            };
            let Some((owner, repo)) = crate::util::split_repo(&repository) else {
                tracing::error!(%repository, "GITHUB_REPOSITORY must be owner/repo");
                return exit::CONFIG;
            };
            let github = GithubClient::new(&token, owner, repo, github_api_base().as_deref());
            modes::pr_event::run(&env, config, &ctx, github).await
        }
        other => {
            tracing::error!(
                mode = other,
                "unknown mode (expected empty, manual or drift-detection)"
            );
            exit::CONFIG
        }
    }
}

/// `--config` / `NIKAMA_CONFIG`, falling back to Digger's `DIGGER_FILENAME`.
fn explicit_config_path(args: &Args) -> Option<PathBuf> {
    args.config
        .clone()
        .or_else(|| crate::util::env_opt("DIGGER_FILENAME").map(PathBuf::from))
}

/// Load the configuration from the working tree. Errors are logged and
/// returned as the exit code.
fn load_local_config(args: &Args, repo_root: &std::path::Path) -> Result<config::Loaded, i32> {
    config::load(repo_root, explicit_config_path(args).as_deref()).map_err(|e| {
        tracing::error!(error = %e, "configuration error");
        exit::CONFIG
    })
}

/// Digger's `reporting-strategy` input (`REPORTING_STRATEGY`) selects the
/// comment mode unless `nikama.yml` sets `comment.mode` itself, which wins
/// because it is part of the (possibly trusted) configuration.
fn apply_reporting_strategy(config: &mut Config) -> Result<(), String> {
    let Some(strategy) = crate::util::env_opt("REPORTING_STRATEGY") else {
        return Ok(());
    };
    let Some(mode) = config::CommentMode::from_reporting_strategy(&strategy) else {
        return Err(format!(
            "unknown REPORTING_STRATEGY '{}' (expected comments_per_run, latest_run_comment or per_project)",
            strategy.trim()
        ));
    };
    if mode == config.comment.mode {
        return Ok(());
    }
    if config.comment.mode_explicit {
        tracing::info!(
            reporting_strategy = %strategy.trim(),
            comment_mode = config.comment.mode.as_str(),
            "comment.mode in the configuration wins over REPORTING_STRATEGY"
        );
    } else {
        tracing::info!(
            reporting_strategy = %strategy.trim(),
            comment_mode = mode.as_str(),
            "comment mode taken from REPORTING_STRATEGY"
        );
        config.comment.mode = mode;
    }
    Ok(())
}

/// Build the Terraform CLI configuration for the provider allowlist from the
/// `allowed-providers` input (`NIKAMA_ALLOWED_PROVIDERS`) and the
/// configuration's `allowed_providers`. `None` when neither is set.
fn provider_cli_config(config: &Config) -> anyhow::Result<Option<String>> {
    use crate::engine::providers;
    let input = match crate::util::env_opt("NIKAMA_ALLOWED_PROVIDERS") {
        Some(s) => {
            let list = providers::parse_list(&s).map_err(|e| anyhow::anyhow!(e))?;
            if list.is_empty() {
                None
            } else {
                Some(list)
            }
        }
        None => None,
    };
    let Some(allowlist) = providers::effective(input.as_deref(), &config.allowed_providers)
        .map_err(|e| anyhow::anyhow!(e))?
    else {
        return Ok(None);
    };
    tracing::info!(
        providers = %allowlist.patterns.join(", "),
        source = allowlist.source,
        "provider allowlist in effect; other providers cannot be installed"
    );
    let existing = providers::existing_cli_config().map_err(|e| {
        anyhow::anyhow!(
            "could not read the existing Terraform CLI configuration: {}",
            e
        )
    })?;
    if existing.is_some() {
        tracing::info!(
            "existing Terraform CLI configuration is kept and extended with the allowlist"
        );
    }
    providers::cli_config(&allowlist.patterns, existing.as_deref())
        .map(Some)
        .map_err(|e| anyhow::anyhow!(e))
}

/// `config-ref` value → git ref. `default` means the repository's default
/// branch, taken from the event payload when present, otherwise from the API.
async fn resolve_config_ref(wanted: &str, github: &GithubClient) -> anyhow::Result<String> {
    let wanted = wanted.trim();
    if !wanted.eq_ignore_ascii_case("default") {
        return Ok(wanted.to_string());
    }
    let from_event = crate::util::env_opt("GITHUB_CONTEXT")
        .and_then(|json| GithubContext::parse(&json).ok())
        .and_then(|ctx| {
            ctx.event["repository"]["default_branch"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
        });
    match from_event {
        Some(b) => Ok(b),
        None => Ok(github.default_branch().await?),
    }
}

fn github_api_base() -> Option<String> {
    crate::util::env_opt("GITHUB_API_URL").filter(|u| !u.contains("api.github.com"))
}

fn print_projects(config: &Config) {
    let header = ["PROJECT", "DIR", "WORKSPACE", "WORKFLOW", "DEPENDS_ON"];
    println!(
        "{:<30} {:<40} {:<10} {:<12} {}",
        header[0], header[1], header[2], header[3], header[4]
    );
    for p in &config.projects {
        println!(
            "{:<30} {:<40} {:<10} {:<12} {}",
            p.name,
            p.dir,
            p.workspace,
            p.workflow,
            p.depends_on.join(",")
        );
    }
}
