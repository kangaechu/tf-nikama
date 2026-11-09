//! Report rendering, comment writer and drift notifier tests.

use super::comment::*;
use super::drift::{DriftNotifier, GithubIssueNotifier, SlackWebhook};
use super::writer::ReportWriter;
use super::COMMENT_MAX_CHARS;
use crate::config::{CommentMode, ShowEmptyPlans};
use crate::engine::{Command, PlanSummary};
use crate::github::GithubClient;
use serde_json::json;
use wiremock::matchers::{body_partial_json, method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const MARKER: &str = "<!-- nikama:run=42:attempt=1 -->";

fn report(command: Command, show: ShowEmptyPlans) -> RunReport {
    let mut r = RunReport::new(MARKER, command, "https://gh/o/r/actions/runs/42", show);
    r.row_mut("b-empty").status = RowStatus::NoChanges;
    r.row_mut("b-empty").detail_kind = Some(DetailKind::Plan);
    r.row_mut("b-empty").detail = Some("No changes.".into());
    {
        let a = r.row_mut("a-changes");
        a.status = RowStatus::Changes;
        a.plan_summary = Some(PlanSummary {
            create: 1,
            update: 2,
            delete: 3,
            replace: 4,
            ..Default::default()
        });
        a.detail_kind = Some(DetailKind::Plan);
        a.detail = Some("plan body".into());
        a.summary_table = Some("| Resource | Action |\n|---|---|\n| `x` | create |\n".into());
        a.additional_output = Some("extra".into());
    }
    r.row_mut("c-locked").status = RowStatus::LockedByOther(7);
    r.notes.push("a note".into());
    r
}

#[test]
fn plan_table_and_blocks() {
    let body = report(Command::Plan, ShowEmptyPlans::Table)
        .render()
        .remove(0);
    assert!(body.starts_with(MARKER));
    assert!(body.contains("### nikama plan · run [#42](https://gh/o/r/actions/runs/42)"));
    assert!(body.contains("· running"));
    assert!(body.contains("| a-changes | 🟡 changes | 1 | 2 | 3 | 4 |"));
    assert!(body.contains("| b-empty | ✅ no changes |"));
    assert!(body.contains("| c-locked | 🔒 locked by #7 |"));
    assert!(body.contains("**Notes**\n- a note"));
    assert!(body.contains("<b>a-changes</b> · plan output (+1 ~2 -3 ±4)"));
    assert!(body.contains("plan summary"));
    assert!(body.contains("additional output"));
    assert!(!body.contains("<b>b-empty</b>"));
    // Rows are sorted by name.
    assert!(body.find("| a-changes").unwrap() < body.find("| b-empty").unwrap());
}

#[test]
fn finished_header_and_empty_plan_modes() {
    let mut r = report(Command::Plan, ShowEmptyPlans::Hide);
    r.finished = true;
    let body = r.render().remove(0);
    assert!(!body.contains("· running"));
    assert!(!body.contains("| b-empty"));

    let body = report(Command::Plan, ShowEmptyPlans::Details)
        .render()
        .remove(0);
    assert!(body.contains("<b>b-empty</b> · plan output"));
}

#[test]
fn apply_table_uses_apply_summary() {
    let mut r = RunReport::new(MARKER, Command::Apply, "u", ShowEmptyPlans::Table);
    let row = r.row_mut("p");
    row.status = RowStatus::Applied;
    row.apply_summary = Some((5, 1, 0));
    row.detail_kind = Some(DetailKind::Apply);
    row.detail = Some("Apply complete!".into());
    let body = r.render().remove(0);
    assert!(body.contains("| Project | Status | + | ~ | - |\n"));
    assert!(body.contains("| p | ✅ applied | 5 | 1 | 0 |"));
    assert!(body.contains("<b>p</b> · apply output"));
}

#[test]
fn error_detail_and_failure_flags() {
    let mut r = RunReport::new(MARKER, Command::Plan, "u", ShowEmptyPlans::Table);
    let row = r.row_mut("p");
    row.status = RowStatus::Failed;
    row.detail_kind = Some(DetailKind::Error);
    row.detail = Some("boom".into());
    assert!(r.any_failed());
    let body = r.render().remove(0);
    assert!(body.contains("<b>p</b> · error"));
    assert!(body.contains("```\nboom\n```"));
    assert!(RowStatus::LockedByOther(1).is_failure());
    assert!(!RowStatus::Skipped("x".into()).is_failure());
    assert!(!RowStatus::Running.is_terminal());
}

#[test]
fn per_project_rendering_has_distinct_markers() {
    let parts = report(Command::Plan, ShowEmptyPlans::Table).render_per_project();
    let names: Vec<&str> = parts.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, vec!["a-changes", "b-empty", "c-locked"]);
    assert!(parts[0]
        .1
        .starts_with("<!-- nikama:run=42:attempt=1 project=a-changes -->"));
    assert!(parts[0].1.contains("| a-changes |"));
    assert!(!parts[0].1.contains("| b-empty |"));
}

#[test]
fn long_details_are_truncated_with_a_log_link() {
    let mut r = RunReport::new(MARKER, Command::Plan, "https://run", ShowEmptyPlans::Table);
    let row = r.row_mut("huge");
    row.status = RowStatus::Changes;
    row.detail_kind = Some(DetailKind::Plan);
    row.detail = Some("y".repeat(200_000));
    let parts = r.render();
    assert_eq!(parts.len(), 1);
    assert!(parts[0].len() <= COMMENT_MAX_CHARS);
    assert!(parts[0].contains("truncated, see the [job log](https://run)"));
}

fn github(server: &MockServer) -> GithubClient {
    GithubClient::new("t", "o", "r", Some(&server.uri()))
}

#[tokio::test]
async fn writer_creates_then_edits_single_comment() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/issues/5/comments"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": 1, "body": "unrelated comment"}
        ])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/o/r/issues/5/comments"))
        .respond_with(
            ResponseTemplate::new(201).set_body_json(json!({"id": 55, "body": "", "html_url": ""})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/repos/o/r/issues/comments/55"))
        .and(body_partial_json(json!({})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 55})))
        .expect(1..)
        .mount(&server)
        .await;

    let mut report = RunReport::new(MARKER, Command::Plan, "u", ShowEmptyPlans::Table);
    report.row_mut("p");
    let (writer, task) = ReportWriter::start(github(&server), 5, report, CommentMode::Single);
    writer.flush().await;
    writer.update(|r| r.row_mut("p").status = RowStatus::Changes);
    writer.finish().await;
    drop(writer);
    task.join().await;

    let reqs = server.received_requests().await.unwrap();
    let posts = reqs.iter().filter(|r| r.method == "POST").count();
    let patches: Vec<_> = reqs.iter().filter(|r| r.method == "PATCH").collect();
    assert_eq!(posts, 1);
    assert!(!patches.is_empty());
    let last: serde_json::Value = serde_json::from_slice(&patches.last().unwrap().body).unwrap();
    let body = last["body"].as_str().unwrap();
    assert!(body.starts_with(MARKER));
    assert!(body.contains("| p | 🟡 changes |"));
    assert!(!body.contains("· running"));
}

#[tokio::test]
async fn writer_reuses_existing_comment_with_marker() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/issues/5/comments"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": 77, "body": format!("{}\nold body", MARKER)}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/repos/o/r/issues/comments/77"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 77})))
        .expect(1..)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path_regex("/repos/o/r/issues/.*"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let report = RunReport::new(MARKER, Command::Plan, "u", ShowEmptyPlans::Table);
    let (writer, task) = ReportWriter::start(github(&server), 5, report, CommentMode::Single);
    writer.finish().await;
    drop(writer);
    task.join().await;
}

#[tokio::test]
async fn writer_per_project_mode_creates_one_comment_per_project() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/issues/5/comments"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/o/r/issues/5/comments"))
        .respond_with(
            ResponseTemplate::new(201).set_body_json(json!({"id": 1, "body": "", "html_url": ""})),
        )
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path_regex("/repos/o/r/issues/comments/.*"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    let mut report = RunReport::new(MARKER, Command::Plan, "u", ShowEmptyPlans::Table);
    report.row_mut("a");
    report.row_mut("b");
    let (writer, task) = ReportWriter::start(github(&server), 5, report, CommentMode::PerProject);
    writer.finish().await;
    drop(writer);
    task.join().await;
}

#[tokio::test]
async fn slack_notifier_splits_long_plans() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/hook"))
        .respond_with(ResponseTemplate::new(200))
        .expect(3)
        .mount(&server)
        .await;
    let plan = (0..1000)
        .map(|i| format!("line {:04}", i))
        .collect::<Vec<_>>()
        .join("\n");
    SlackWebhook::new(&format!("{}/hook", server.uri()))
        .notify("proj", "o/r", &plan)
        .await
        .unwrap();
    let reqs = server.received_requests().await.unwrap();
    let first: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
    assert!(first["text"]
        .as_str()
        .unwrap()
        .contains("Infrastructure Drift Detected"));
    assert!(first["text"].as_str().unwrap().contains("*Project:* proj"));
}

#[tokio::test]
async fn slack_notifier_reports_http_errors() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let err = SlackWebhook::new(&server.uri())
        .notify_error("p", "o/r", "boom")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("500"));
}

#[tokio::test]
async fn issue_notifier_creates_or_updates() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/issues"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"number": 3, "title": "Drift detected in project: existing", "html_url": ""}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/o/r/issues"))
        .and(body_partial_json(
            json!({"title": "Drift detected in project: fresh", "labels": ["digger"]}),
        ))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_json(json!({"number": 4, "title": "t", "html_url": ""})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/repos/o/r/issues/3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let n = GithubIssueNotifier::new(github(&server));
    n.notify("fresh", "o/r", "plan").await.unwrap();
    n.notify("existing", "o/r", "plan").await.unwrap();
}
