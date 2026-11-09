//! Interpretation of `terraform show -json` plans and apply output.

use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlanSummary {
    pub create: u32,
    pub update: u32,
    pub delete: u32,
    pub replace: u32,
    pub output_changes: u32,
    /// Per-resource rows: (address, action label).
    pub rows: Vec<(String, String)>,
}

impl PlanSummary {
    pub fn is_empty(&self) -> bool {
        self.create == 0
            && self.update == 0
            && self.delete == 0
            && self.replace == 0
            && self.output_changes == 0
    }
    pub fn short(&self) -> String {
        format!(
            "+{} ~{} -{} ±{}",
            self.create, self.update, self.delete, self.replace
        )
    }
}

#[derive(Debug, Deserialize)]
struct TfPlan {
    #[serde(default)]
    resource_changes: Vec<ResourceChange>,
    #[serde(default)]
    output_changes: BTreeMap<String, Change>,
}

#[derive(Debug, Deserialize)]
struct ResourceChange {
    address: String,
    change: Change,
}

#[derive(Debug, Deserialize)]
struct Change {
    #[serde(default)]
    actions: Vec<String>,
}

fn classify(actions: &[String]) -> Option<&'static str> {
    let a: Vec<&str> = actions.iter().map(|s| s.as_str()).collect();
    match a.as_slice() {
        [] | ["no-op"] => None,
        ["read"] => None,
        ["create"] => Some("create"),
        ["update"] => Some("update"),
        ["delete"] => Some("delete"),
        ["delete", "create"] | ["create", "delete"] => Some("replace"),
        _ => Some("update"),
    }
}

/// Parse a plan JSON document into a summary.
pub fn plan_summary_from_json(json: &str) -> Result<PlanSummary, serde_json::Error> {
    let plan: TfPlan = serde_json::from_str(json)?;
    let mut s = PlanSummary::default();
    for rc in &plan.resource_changes {
        if let Some(kind) = classify(&rc.change.actions) {
            match kind {
                "create" => s.create += 1,
                "update" => s.update += 1,
                "delete" => s.delete += 1,
                "replace" => s.replace += 1,
                _ => {}
            }
            s.rows.push((rc.address.clone(), kind.to_string()));
        }
    }
    for c in plan.output_changes.values() {
        if classify(&c.actions).is_some() {
            s.output_changes += 1;
        }
    }
    Ok(s)
}

/// Markdown table of the resource changes.
pub fn plan_summary_table(summary: &PlanSummary) -> String {
    if summary.rows.is_empty() {
        return String::new();
    }
    let mut out = String::from("| Resource | Action |\n|---|---|\n");
    for (addr, action) in &summary.rows {
        let sym = match action.as_str() {
            "create" => "➕ create",
            "update" => "🔄 update",
            "delete" => "❌ delete",
            "replace" => "♻️ replace",
            other => other,
        };
        out.push_str(&format!("| `{}` | {} |\n", addr, sym));
    }
    if summary.output_changes > 0 {
        out.push_str(&format!(
            "| _outputs_ | {} changed |\n",
            summary.output_changes
        ));
    }
    out
}

/// `(added, changed, destroyed)` parsed from apply output; last match wins.
pub fn apply_summary_from_output(output: &str) -> Option<(u32, u32, u32)> {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE
        .get_or_init(|| regex::Regex::new(r"(\d+) added, (\d+) changed, (\d+) destroyed").unwrap());
    let mut last = None;
    for cap in re.captures_iter(output) {
        last = Some((
            cap[1].parse().unwrap_or(0),
            cap[2].parse().unwrap_or(0),
            cap[3].parse().unwrap_or(0),
        ));
    }
    last
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarizes_plan() {
        let json = r#"{
          "resource_changes": [
            {"address": "aws_s3_bucket.a", "change": {"actions": ["create"]}},
            {"address": "aws_s3_bucket.b", "change": {"actions": ["no-op"]}},
            {"address": "aws_instance.c", "change": {"actions": ["delete", "create"]}},
            {"address": "aws_instance.d", "change": {"actions": ["update"]}},
            {"address": "aws_instance.e", "change": {"actions": ["delete"]}}
          ],
          "output_changes": {"x": {"actions": ["update"]}, "y": {"actions": ["no-op"]}}
        }"#;
        let s = plan_summary_from_json(json).unwrap();
        assert_eq!(
            (s.create, s.update, s.delete, s.replace, s.output_changes),
            (1, 1, 1, 1, 1)
        );
        assert!(!s.is_empty());
        assert!(plan_summary_table(&s).contains("aws_instance.c"));
    }

    #[test]
    fn empty_plan() {
        let s = plan_summary_from_json(
            r#"{"resource_changes": [{"address":"a","change":{"actions":["no-op"]}}]}"#,
        )
        .unwrap();
        assert!(s.is_empty());
    }

    #[test]
    fn apply_summary() {
        assert_eq!(
            apply_summary_from_output(
                "Apply complete! Resources: 1 added, 2 changed, 3 destroyed."
            ),
            Some((1, 2, 3))
        );
        assert_eq!(apply_summary_from_output("nothing"), None);
    }
}
