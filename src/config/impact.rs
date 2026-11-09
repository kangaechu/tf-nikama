//! Changed-file → impacted-project matching.

use super::{Config, DependencyMode, Project};
use globset::GlobSetBuilder;

/// Normalise a repository-relative path or pattern: strip `./` and leading
/// slashes, collapse `.` segments, resolve `..` where possible.
pub fn normalize_path(p: &str) -> String {
    let normalized = p.replace('\\', "/");
    let mut parts: Vec<&str> = Vec::new();
    for seg in normalized.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

fn join_pattern(dir: &str, pattern: &str) -> String {
    // Patterns that start with `.` are relative to the project dir (Digger
    // behaviour); everything else is repository-relative.
    if pattern.starts_with('.') {
        normalize_path(&format!("{}/{}", dir, pattern))
    } else {
        normalize_path(pattern)
    }
}

fn build_set(patterns: &[String]) -> Result<globset::GlobSet, globset::Error> {
    let mut b = GlobSetBuilder::new();
    for p in patterns {
        let mut g = globset::GlobBuilder::new(p);
        g.literal_separator(true);
        b.add(g.build()?);
    }
    b.build()
}

/// `true` if `file` matches any include (or includes is empty) and no exclude.
pub fn matches_patterns(file: &str, includes: &[String], excludes: &[String]) -> bool {
    let file = normalize_path(file);
    let inc: Vec<String> = includes.iter().map(|p| normalize_path(p)).collect();
    let exc: Vec<String> = excludes.iter().map(|p| normalize_path(p)).collect();
    let inc_set = match build_set(&inc) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "invalid include pattern; treating as no match");
            return false;
        }
    };
    let exc_set = match build_set(&exc) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "invalid exclude pattern; ignoring excludes");
            return inc.is_empty() || inc_set.is_match(&file);
        }
    };
    let included = inc.is_empty() || inc_set.is_match(&file);
    let excluded = !exc.is_empty() && exc_set.is_match(&file);
    included && !excluded
}

/// Projects impacted by `changed_files`, in configuration order. Honours
/// `dependency_configuration.mode`.
pub fn impacted_projects(config: &Config, changed_files: &[String]) -> Vec<Project> {
    let mut impacted: Vec<Project> = Vec::new();
    for p in &config.projects {
        let dir = normalize_path(&p.dir);
        let mut includes: Vec<String> = vec![if dir.is_empty() {
            "**/*".to_string()
        } else {
            format!("{}/**/*", dir)
        }];
        includes.extend(p.include_patterns.iter().map(|pat| join_pattern(&dir, pat)));
        let excludes: Vec<String> = p
            .exclude_patterns
            .iter()
            .map(|pat| join_pattern(&dir, pat))
            .collect();
        if changed_files
            .iter()
            .any(|f| matches_patterns(f, &includes, &excludes))
        {
            impacted.push(p.clone());
        }
    }
    if config.dependency_mode == DependencyMode::Hard {
        let names: Vec<String> = impacted.iter().map(|p| p.name.clone()).collect();
        for d in config.graph.descendants(&names) {
            if let Some(p) = config.project(&d) {
                impacted.push(p.clone());
            }
        }
        // Keep configuration order.
        let order: std::collections::HashMap<&str, usize> = config
            .projects
            .iter()
            .enumerate()
            .map(|(i, p)| (p.name.as_str(), i))
            .collect();
        impacted.sort_by_key(|p| order[p.name.as_str()]);
    }
    impacted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes() {
        assert_eq!(normalize_path("./a/./b/../c"), "a/c");
        assert_eq!(normalize_path("/a/b/"), "a/b");
    }

    #[test]
    fn matches() {
        assert!(matches_patterns(
            "dev/vpc/main.tf",
            &["dev/**/*".into()],
            &[]
        ));
        assert!(!matches_patterns(
            "prod/vpc/main.tf",
            &["dev/**/*".into()],
            &[]
        ));
        assert!(!matches_patterns(
            "dev/vpc/README.md",
            &["dev/**/*".into()],
            &["**/*.md".into()]
        ));
        // `*` does not cross directory boundaries.
        assert!(!matches_patterns("dev/vpc/main.tf", &["dev/*".into()], &[]));
        assert!(matches_patterns("anything", &[], &[]));
    }
}
