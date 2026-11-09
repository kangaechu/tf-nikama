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

    /// Log level: ERROR, WARN, INFO, DEBUG
    #[arg(long, env = "NIKAMA_LOG_LEVEL", default_value = "INFO")]
    pub log_level: String,

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

fn init_logging(level: &str) {
    let level = match crate::util::env_first(&["NIKAMA_LOG_LEVEL", "DIGGER_LOG_LEVEL"])
        .unwrap_or_else(|| level.to_string())
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
        .add_directive("rustls=warn".parse().unwrap());
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stdout)
        .init();
}

pub async fn run() -> i32 {
    let args = Args::parse();
    init_logging(&args.log_level);
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "nikama starting");

    let repo_root = args
        .repo_root
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));

    // Configuration.
    let loaded = match config::load(&repo_root, args.config.as_deref()) {
        Ok(l) => l,
        Err(e) => {
            // Legacy env var for an explicit file name.
            if args.config.is_none() {
                if let Some(legacy) = crate::util::env_opt("DIGGER_FILENAME") {
                    match config::load(&repo_root, Some(std::path::Path::new(&legacy))) {
                        Ok(l) => l,
                        Err(e2) => {
                            tracing::error!(error = %e2, "configuration error");
                            return exit::CONFIG;
                        }
                    }
                } else {
                    tracing::error!(error = %e, "configuration error");
                    return exit::CONFIG;
                }
            } else {
                tracing::error!(error = %e, "configuration error");
                return exit::CONFIG;
            }
        }
    };
    if loaded.legacy_name {
        tracing::warn!(
            path = %loaded.path.display(),
            "using legacy configuration file name; please rename it to nikama.yml"
        );
    }
    let config = Arc::new(loaded.config);
    tracing::info!(
        path = %loaded.path.display(),
        projects = config.projects.len(),
        workflows = config.workflows.len(),
        "configuration loaded"
    );
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
    };

    let mode = crate::util::env_first(&["INPUT_DIGGER_MODE", "NIKAMA_MODE"])
        .unwrap_or_default()
        .to_ascii_lowercase();

    // GitHub identity.
    let token =
        crate::util::env_first(&["DIGGER_GITHUB_TOKEN", "NIKAMA_GITHUB_TOKEN", "GITHUB_TOKEN"]);
    let repository = crate::util::env_opt("GITHUB_REPOSITORY");
    let actor = crate::util::env_opt("GITHUB_ACTOR").unwrap_or_else(|| "unknown".to_string());

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
