//! Single-comment run report model and Markdown rendering.

use super::COMMENT_MAX_CHARS;
use crate::config::ShowEmptyPlans;
use crate::engine::{ApplySummary, Command, PlanSummary};
use crate::util::truncate_chars;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowStatus {
    Pending,
    Running,
    NoChanges,
    Changes,
    Applied,
    Destroyed,
    Locked,
    Unlocked,
    Failed,
    /// Held by another PR.
    LockedByOther(u64),
    Skipped(String),
}

impl RowStatus {
    pub fn label(&self) -> String {
        match self {
            RowStatus::Pending => "⏳ pending".into(),
            RowStatus::Running => "🔄 running".into(),
            RowStatus::NoChanges => "✅ no changes".into(),
            RowStatus::Changes => "🟡 changes".into(),
            RowStatus::Applied => "✅ applied".into(),
            RowStatus::Destroyed => "💥 destroyed".into(),
            RowStatus::Locked => "🔒 locked".into(),
            RowStatus::Unlocked => "🔓 unlocked".into(),
            RowStatus::Failed => "❌ failed".into(),
            RowStatus::LockedByOther(pr) => format!("🔒 locked by #{}", pr),
            RowStatus::Skipped(reason) => format!("⏭️ skipped ({})", reason),
        }
    }

    pub fn is_failure(&self) -> bool {
        matches!(self, RowStatus::Failed | RowStatus::LockedByOther(_))
    }

    pub fn is_terminal(&self) -> bool {
        !matches!(self, RowStatus::Pending | RowStatus::Running)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailKind {
    Plan,
    Apply,
    Error,
}

#[derive(Debug, Clone)]
pub struct ProjectRow {
    pub name: String,
    pub status: RowStatus,
    pub plan_summary: Option<PlanSummary>,
    pub apply_summary: Option<ApplySummary>,
    pub detail_kind: Option<DetailKind>,
    pub detail: Option<String>,
    pub summary_table: Option<String>,
    pub additional_output: Option<String>,
}

impl ProjectRow {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            status: RowStatus::Pending,
            plan_summary: None,
            apply_summary: None,
            detail_kind: None,
            detail: None,
            summary_table: None,
            additional_output: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RunReport {
    pub marker: String,
    pub command: Command,
    pub run_url: String,
    pub started_at: String,
    pub rows: BTreeMap<String, ProjectRow>,
    pub notes: Vec<String>,
    pub show_empty_plans: ShowEmptyPlans,
    pub finished: bool,
    /// Wakeup word used in the command hints of the footer.
    pub wakeup_word: String,
}

impl RunReport {
    pub fn new(
        marker: &str,
        command: Command,
        run_url: &str,
        show_empty_plans: ShowEmptyPlans,
        wakeup_word: &str,
    ) -> Self {
        Self {
            marker: marker.to_string(),
            command,
            run_url: run_url.to_string(),
            started_at: chrono::Utc::now().format("%Y-%m-%d %H:%M UTC").to_string(),
            rows: BTreeMap::new(),
            notes: Vec::new(),
            show_empty_plans,
            finished: false,
            wakeup_word: wakeup_word.to_string(),
        }
    }

    pub fn row_mut(&mut self, name: &str) -> &mut ProjectRow {
        self.rows
            .entry(name.to_string())
            .or_insert_with(|| ProjectRow::new(name))
    }

    pub fn any_failed(&self) -> bool {
        self.rows.values().any(|r| r.status.is_failure())
    }

    fn header(&self, part: Option<(usize, usize)>) -> String {
        let run_id = self.run_url.rsplit('/').next().unwrap_or("").to_string();
        let mut s = format!("{}\n", self.marker_for(part.map(|p| p.0)));
        let suffix = match part {
            Some((i, n)) if n > 1 => format!(" · part {}/{}", i, n),
            _ => String::new(),
        };
        let state = if self.finished { "" } else { " · running" };
        s.push_str(&format!(
            "### nikama {} · run [#{}]({}) · {}{}{}\n\n",
            self.command, run_id, self.run_url, self.started_at, state, suffix
        ));
        // Record every project, including empty plans omitted from the table.
        // Each split part carries the full scope so it cannot be hidden while
        // another project from the same report is still current.
        let metadata = serde_json::json!({
            "command": self.command.to_string(),
            "projects": self.rows.keys().collect::<Vec<_>>(),
        })
        .to_string()
        .replace('<', "\\u003c")
        .replace('>', "\\u003e");
        s.push_str(&format!("<!-- nikama:scope {} -->\n\n", metadata));
        s
    }

    fn marker_for(&self, part: Option<usize>) -> String {
        match part {
            Some(i) if i > 1 => format!(
                "{} part={} -->",
                self.marker.trim_end_matches("-->").trim_end(),
                i
            ),
            _ => self.marker.clone(),
        }
    }

    fn table(&self) -> String {
        let is_apply = matches!(self.command, Command::Apply | Command::Destroy);
        let visible = || {
            self.rows.values().filter(|row| {
                !(self.show_empty_plans == ShowEmptyPlans::Hide
                    && row.status == RowStatus::NoChanges)
            })
        };
        // The import column is only shown when some project imports a
        // resource, so the usual layout stays unchanged.
        let with_import = visible().any(|row| {
            if is_apply {
                row.apply_summary.is_some_and(|a| a.imported > 0)
            } else {
                row.plan_summary.as_ref().is_some_and(|p| p.import > 0)
            }
        });
        let mut s = String::new();
        let mut cols: Vec<&str> = if is_apply {
            vec!["+", "~", "-"]
        } else {
            vec!["+", "~", "-", "±"]
        };
        if with_import {
            cols.push("↓");
        }
        s.push_str(&format!("| Project | Status | {} |\n", cols.join(" | ")));
        s.push_str(&format!("|---|---|{}\n", "---|".repeat(cols.len())));
        for row in visible() {
            let mut cells: Vec<String> = if is_apply {
                row.apply_summary
                    .map(|a| {
                        let mut v = vec![a.added, a.changed, a.destroyed];
                        if with_import {
                            v.push(a.imported);
                        }
                        v.iter().map(u32::to_string).collect()
                    })
                    .unwrap_or_default()
            } else {
                row.plan_summary
                    .as_ref()
                    .map(|p| {
                        let mut v = vec![p.create, p.update, p.delete, p.replace];
                        if with_import {
                            v.push(p.import);
                        }
                        v.iter().map(u32::to_string).collect()
                    })
                    .unwrap_or_default()
            };
            cells.resize(cols.len(), String::new());
            s.push_str(&format!(
                "| {} | {} | {} |\n",
                row.name,
                row.status.label(),
                cells.join(" | ")
            ));
        }
        s
    }

    fn notes_block(&self) -> String {
        if self.notes.is_empty() {
            return String::new();
        }
        let mut s = String::from("\n**Notes**\n");
        for n in &self.notes {
            s.push_str(&format!("- {}\n", n));
        }
        s
    }

    fn footer(&self) -> String {
        let w = &self.wakeup_word;
        format!("\n---\n`{w} apply` · `{w} apply -p <project>` · `{w} unlock` · `{w} help`\n")
    }

    /// Detail block for a row, given a character budget for the detail body.
    fn detail_block(&self, row: &ProjectRow, budget: usize) -> Option<String> {
        let (kind, detail) = match (&row.detail_kind, &row.detail) {
            (Some(k), Some(d)) => (k, d),
            _ => return None,
        };
        if row.status == RowStatus::NoChanges && self.show_empty_plans != ShowEmptyPlans::Details {
            return None;
        }
        let title = match kind {
            DetailKind::Plan => format!(
                "<b>{}</b> · plan output{}",
                row.name,
                row.plan_summary
                    .as_ref()
                    .map(|s| format!(" ({})", s.short()))
                    .unwrap_or_default()
            ),
            DetailKind::Apply => format!("<b>{}</b> · apply output", row.name),
            DetailKind::Error => format!("<b>{}</b> · error", row.name),
        };
        let truncated_note = format!("\n... (truncated, see the [job log]({}))", self.run_url);
        let body = truncate_chars(detail.trim(), budget, &truncated_note);
        let lang = match kind {
            DetailKind::Plan | DetailKind::Apply => "terraform",
            DetailKind::Error => "",
        };
        let mut s = format!(
            "<details><summary>{}</summary>\n\n```{}\n{}\n```\n",
            title, lang, body
        );
        if let Some(table) = &row.summary_table {
            let title = match kind {
                DetailKind::Plan => Some("plan summary"),
                // The plan produced right before an apply without plan storage.
                DetailKind::Apply => Some("plan applied (planned immediately before apply)"),
                DetailKind::Error => None,
            };
            if let (Some(title), false) = (title, table.is_empty()) {
                s.push_str(&format!(
                    "\n<details><summary>{}</summary>\n\n{}\n</details>\n",
                    title, table
                ));
            }
        }
        if let Some(extra) = &row.additional_output {
            s.push_str(&format!(
                "\n<details><summary>additional output</summary>\n\n```\n{}\n```\n</details>\n",
                truncate_chars(extra, 4000, "\n... (truncated)")
            ));
        }
        s.push_str("</details>\n");
        Some(s)
    }

    /// Render into one or more comment bodies, each within the size limit.
    pub fn render(&self) -> Vec<String> {
        const MARGIN: usize = 2_000;
        let header = self.header(None);
        let fixed =
            header.len() + self.table().len() + self.notes_block().len() + self.footer().len();
        let budget_total = COMMENT_MAX_CHARS.saturating_sub(fixed + MARGIN);

        let detail_rows: Vec<&ProjectRow> = self
            .rows
            .values()
            .filter(|r| r.detail.is_some())
            .filter(|r| {
                !(r.status == RowStatus::NoChanges
                    && self.show_empty_plans != ShowEmptyPlans::Details)
            })
            .collect();
        // Each project gets an equal share of the budget, but never less than
        // MIN_DETAIL chars: beyond that point we prefer spilling into extra
        // comments over truncating plans into uselessness.
        const MIN_DETAIL: usize = 12_000;
        let per_row_budget = if detail_rows.is_empty() {
            budget_total
        } else {
            (budget_total / detail_rows.len()).max(MIN_DETAIL)
        };

        let blocks: Vec<String> = detail_rows
            .iter()
            .filter_map(|r| self.detail_block(r, per_row_budget))
            .collect();

        // Pack blocks into parts.
        let mut parts: Vec<Vec<String>> = vec![vec![]];
        let mut current_len = fixed;
        for b in blocks {
            if current_len + b.len() > COMMENT_MAX_CHARS.saturating_sub(MARGIN)
                && !parts.last().unwrap().is_empty()
            {
                parts.push(vec![]);
                current_len = header.len() + 200;
            }
            current_len += b.len();
            parts.last_mut().unwrap().push(b);
        }
        let n = parts.len();
        parts
            .into_iter()
            .enumerate()
            .map(|(i, blocks)| {
                let mut s = self.header(Some((i + 1, n)));
                if i == 0 {
                    s.push_str(&self.table());
                    s.push_str(&self.notes_block());
                }
                if !blocks.is_empty() {
                    s.push('\n');
                    s.push_str(&blocks.join("\n"));
                }
                if i == 0 {
                    s.push_str(&self.footer());
                }
                s
            })
            .collect()
    }

    /// Per-project rendering for `comment.mode: per_project`.
    pub fn render_per_project(&self) -> Vec<(String, String)> {
        self.rows
            .values()
            .map(|row| {
                let mut single = self.clone();
                single.rows = BTreeMap::from([(row.name.clone(), row.clone())]);
                single.marker = format!(
                    "{} project={} -->",
                    self.marker.trim_end_matches("-->").trim_end(),
                    row.name
                );
                (row.name.clone(), single.render().remove(0))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> RunReport {
        let mut r = RunReport::new(
            "<!-- nikama:run=1:attempt=1 -->",
            Command::Plan,
            "https://github.com/o/r/actions/runs/1",
            ShowEmptyPlans::Table,
            "nikama",
        );
        {
            let a = r.row_mut("app");
            a.status = RowStatus::Changes;
            a.plan_summary = Some(PlanSummary {
                create: 2,
                update: 1,
                ..Default::default()
            });
            a.detail_kind = Some(DetailKind::Plan);
            a.detail = Some("Terraform will perform the following actions:\n + x".into());
        }
        {
            let n = r.row_mut("net");
            n.status = RowStatus::NoChanges;
            n.detail_kind = Some(DetailKind::Plan);
            n.detail = Some("No changes.".into());
        }
        r
    }

    #[test]
    fn renders_single_part() {
        let parts = report().render();
        assert_eq!(parts.len(), 1);
        let body = &parts[0];
        assert!(body.starts_with("<!-- nikama:run=1:attempt=1 -->"));
        assert!(body.contains("| app | 🟡 changes | 2 | 1 | 0 | 0 |"));
        assert!(body.contains("| net | ✅ no changes |"));
        assert!(body.contains("<b>app</b> · plan output"));
        // Empty plans do not get a details block.
        assert!(!body.contains("<b>net</b>"));
        assert!(body.contains("`nikama apply`"));
    }

    #[test]
    fn import_column_only_when_imports_exist() {
        let mut r = report();
        assert!(r.render()[0].contains("| Project | Status | + | ~ | - | ± |\n"));
        {
            let row = r.row_mut("iam");
            row.status = RowStatus::Changes;
            row.plan_summary = Some(PlanSummary {
                import: 67,
                ..Default::default()
            });
        }
        let body = r.render().remove(0);
        assert!(body.contains("| Project | Status | + | ~ | - | ± | ↓ |\n"));
        assert!(body.contains("|---|---|---|---|---|---|---|\n"));
        assert!(body.contains("| iam | 🟡 changes | 0 | 0 | 0 | 0 | 67 |"));
        assert!(body.contains("| app | 🟡 changes | 2 | 1 | 0 | 0 | 0 |"));
        assert!(body.contains("| net | ✅ no changes |  |  |  |  |  |"));
    }

    #[test]
    fn splits_when_too_big() {
        let mut r = report();
        for i in 0..6 {
            let row = r.row_mut(&format!("big{}", i));
            row.status = RowStatus::Changes;
            row.detail_kind = Some(DetailKind::Plan);
            row.detail = Some("x".repeat(40_000));
        }
        let parts = r.render();
        assert!(
            parts.len() >= 2,
            "expected multiple parts, got {}",
            parts.len()
        );
        for p in &parts {
            assert!(p.len() <= COMMENT_MAX_CHARS, "part too large: {}", p.len());
        }
        assert!(parts[1].contains("part=2 -->"));
    }
}
