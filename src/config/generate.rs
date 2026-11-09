//! `generate_projects` expansion for Terraform / OpenTofu directories.

use super::convert::{convert_aws_role, convert_cognito};
use super::impact::{matches_patterns, normalize_path};
use super::raw::RawGenerateProjects;
use super::{ConfigError, Project, DEFAULT_WORKFLOW, DEFAULT_WORKSPACE};
use std::path::Path;

/// Directories (repository-relative) that contain at least one `*.tf` file.
/// Directories named `modules` and hidden directories (`.terraform`, `.git`)
/// are skipped. When `traverse_nested` is false, discovery does not descend
/// into a directory that is itself a project.
pub fn discover_terraform_dirs(root: &Path, traverse_nested: bool) -> Vec<String> {
    let mut out = Vec::new();
    let mut walker = walkdir::WalkDir::new(root)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter();
    while let Some(entry) = walker.next() {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(error = %e, "error while walking repository");
                continue;
            }
        };
        if !entry.file_type().is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy();
        if entry.depth() > 0 && (name == "modules" || name.starts_with('.')) {
            walker.skip_current_dir();
            continue;
        }
        let has_tf = std::fs::read_dir(entry.path())
            .map(|rd| {
                rd.filter_map(|e| e.ok()).any(|e| {
                    e.file_type().map(|t| t.is_file()).unwrap_or(false)
                        && e.file_name().to_string_lossy().ends_with(".tf")
                })
            })
            .unwrap_or(false);
        if has_tf {
            let rel = entry
                .path()
                .strip_prefix(root)
                .unwrap_or(entry.path())
                .to_string_lossy()
                .replace('\\', "/");
            out.push(if rel.is_empty() { ".".to_string() } else { rel });
            if !traverse_nested {
                walker.skip_current_dir();
            }
        }
    }
    out
}

fn dir_matches(dir: &str, include: Option<&str>, exclude: Option<&str>) -> bool {
    let include = include.map(|s| s.trim()).filter(|s| !s.is_empty());
    let exclude = exclude.map(|s| s.trim()).filter(|s| !s.is_empty());
    // Digger quirk: an empty include matches nothing. We keep that: a block
    // without include generates nothing.
    let Some(include) = include else {
        return false;
    };
    // Try the directory itself and a representative file inside it so that
    // both `dev/**` and `dev/**/*.tf`-style patterns work.
    let probe_file = format!("{}/main.tf", dir);
    let inc = vec![include.to_string()];
    let exc: Vec<String> = exclude.map(|s| vec![s.to_string()]).unwrap_or_default();
    let included = matches_patterns(dir, &inc, &[]) || matches_patterns(&probe_file, &inc, &[]);
    // `matches_patterns` with no includes returns true unless excluded.
    let excluded = !exc.is_empty()
        && (!matches_patterns(dir, &[], &exc) || !matches_patterns(&probe_file, &[], &exc));
    included && !excluded
}

pub(super) fn generate_projects(
    gen: &RawGenerateProjects,
    root: &Path,
    traverse_nested: bool,
) -> Result<Vec<Project>, ConfigError> {
    let has_top = gen.include.is_some() || gen.exclude.is_some();
    if has_top && !gen.blocks.is_empty() {
        return Err(ConfigError::Validation(
            "generate_projects: use either include/exclude or blocks, not both".to_string(),
        ));
    }
    let dirs = discover_terraform_dirs(root, traverse_nested);
    let mut projects = Vec::new();

    if has_top {
        let aws_role = gen.aws_role_to_assume.as_ref().map(convert_aws_role);
        let cognito = gen
            .aws_cognito_oidc
            .as_ref()
            .map(|c| convert_cognito(c, "generate_projects"))
            .transpose()?;
        for dir in &dirs {
            if dir_matches(dir, gen.include.as_deref(), gen.exclude.as_deref()) {
                projects.push(Project {
                    name: project_name_for_dir(dir),
                    dir: dir.clone(),
                    workspace: DEFAULT_WORKSPACE.to_string(),
                    workflow: DEFAULT_WORKFLOW.to_string(),
                    opentofu: false,
                    include_patterns: vec![],
                    exclude_patterns: vec![],
                    depends_on: vec![],
                    layer: 0,
                    alias: None,
                    block_name: None,
                    branch: None,
                    apply_requirements: vec!["mergeable".to_string()],
                    drift_detection: true,
                    drift_include_patterns: vec![],
                    drift_exclude_patterns: vec![],
                    aws_role_to_assume: aws_role.clone(),
                    aws_cognito_oidc: cognito.clone(),
                    generated: true,
                });
            }
        }
    }

    for (i, block) in gen.blocks.iter().enumerate() {
        let ctx = format!("generate_projects.blocks[{}]", i);
        let aws_role = block
            .aws_role_to_assume
            .as_ref()
            .or(gen.aws_role_to_assume.as_ref())
            .map(convert_aws_role);
        let cognito = block
            .aws_cognito_oidc
            .as_ref()
            .or(gen.aws_cognito_oidc.as_ref())
            .map(|c| convert_cognito(c, &ctx))
            .transpose()?;
        if block
            .include
            .as_deref()
            .map(|s| s.trim().is_empty())
            .unwrap_or(true)
        {
            return Err(ConfigError::Validation(format!(
                "{}: include is required for a Terraform/OpenTofu block",
                ctx
            )));
        }
        for dir in &dirs {
            if !dir_matches(dir, block.include.as_deref(), block.exclude.as_deref()) {
                continue;
            }
            projects.push(Project {
                name: project_name_for_dir(dir),
                dir: dir.clone(),
                workspace: block
                    .workspace
                    .clone()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| DEFAULT_WORKSPACE.to_string()),
                workflow: block
                    .workflow
                    .clone()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| DEFAULT_WORKFLOW.to_string()),
                opentofu: block.opentofu.unwrap_or(false),
                include_patterns: block.include_patterns.clone(),
                exclude_patterns: block.exclude_patterns.clone(),
                depends_on: vec![],
                layer: 0,
                alias: None,
                block_name: block.block_name.clone().filter(|s| !s.is_empty()),
                branch: None,
                apply_requirements: vec!["mergeable".to_string()],
                drift_detection: true,
                drift_include_patterns: block.drift_include_patterns.clone(),
                drift_exclude_patterns: block.drift_exclude_patterns.clone(),
                aws_role_to_assume: aws_role.clone(),
                aws_cognito_oidc: cognito.clone(),
                generated: true,
            });
        }
    }
    Ok(projects)
}

fn project_name_for_dir(dir: &str) -> String {
    let n = normalize_path(dir).replace('/', "_");
    if n.is_empty() {
        "root".to_string()
    } else {
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_and_matches() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for d in [
            "dev/vpc",
            "dev/app",
            "prod/vpc",
            "modules/shared",
            "dev/app/modules/x",
        ] {
            std::fs::create_dir_all(root.join(d)).unwrap();
            std::fs::write(root.join(d).join("main.tf"), "").unwrap();
        }
        std::fs::create_dir_all(root.join("docs")).unwrap();
        let dirs = discover_terraform_dirs(root, true);
        assert_eq!(dirs, vec!["dev/app", "dev/vpc", "prod/vpc"]);
        assert!(dir_matches("dev/vpc", Some("dev/**"), None));
        assert!(!dir_matches("prod/vpc", Some("dev/**"), None));
        assert!(!dir_matches("dev/app", Some("dev/**"), Some("dev/app")));
        assert!(!dir_matches("dev/app", Some("dev/**"), Some("dev/app/**")));
        assert_eq!(project_name_for_dir("dev/vpc"), "dev_vpc");
    }
}
