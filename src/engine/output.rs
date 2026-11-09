//! Human-readable plan / apply output trimming.

use crate::util::strip_ansi;

const PLAN_MARKERS: [&str; 3] = [
    "Terraform will perform the following actions:",
    "OpenTofu will perform the following actions:",
    "No changes.",
];

/// Extract the interesting part of a `plan` stdout: from the "will perform the
/// following actions" marker (or "No changes.") to the end, dropping the
/// trailing separator line and the "Saved the plan to" trailer.
pub fn cleanup_plan_output(stdout: &str) -> String {
    let text = strip_ansi(stdout);
    let start = PLAN_MARKERS
        .iter()
        .filter_map(|m| text.find(m))
        .min()
        .unwrap_or(0);
    let body = &text[start..];
    let end = body
        .find("─────")
        .or_else(|| body.find("Saved the plan to:"))
        .unwrap_or(body.len());
    body[..end].trim().to_string()
}

/// Apply output: drop everything before the first resource action line when
/// an init preamble is present.
pub fn cleanup_apply_output(stdout: &str) -> String {
    let text = strip_ansi(stdout);
    let start = text
        .find("Initializing the backend...")
        .map(|_| {
            text.find("Terraform will perform the following actions:")
                .or_else(|| text.find("OpenTofu will perform the following actions:"))
                .or_else(|| text.find("Apply complete!"))
                .unwrap_or(0)
        })
        .unwrap_or(0);
    text[start..].trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_plan() {
        let raw = "Refreshing state...\n\nTerraform will perform the following actions:\n\n  # x will be created\n\nPlan: 1 to add.\n\n─────────────────────\n\nSaved the plan to: x.tfplan\n";
        let out = cleanup_plan_output(raw);
        assert!(out.starts_with("Terraform will perform"));
        assert!(out.ends_with("Plan: 1 to add."));
    }

    #[test]
    fn no_changes() {
        let raw = "\x1b[0mNo changes. Your infrastructure matches the configuration.\n";
        assert_eq!(
            cleanup_plan_output(raw),
            "No changes. Your infrastructure matches the configuration."
        );
    }
}
