//! GitHub REST client tests against a local mock server.

use super::client::{CheckKind, GithubClient};
use serde_json::json;
use wiremock::matchers::{body_json, body_partial_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn client(server: &MockServer) -> GithubClient {
    GithubClient::new("tok-123", "o", "r", Some(&server.uri()))
}

#[tokio::test]
async fn changed_files_paginate_and_include_renames() {
    let server = MockServer::start().await;
    let page1: Vec<_> = (0..100)
        .map(|i| {
            if i == 5 {
                json!({"filename": "new/name.tf", "previous_filename": "old/name.tf"})
            } else {
                json!({"filename": format!("f{}.tf", i)})
            }
        })
        .collect();
    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls/7/files"))
        .and(query_param("page", "1"))
        .and(header("authorization", "Bearer tok-123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page1))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls/7/files"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"filename": "last.tf"}])))
        .mount(&server)
        .await;

    let files = client(&server).await.changed_files(7).await.unwrap();
    assert_eq!(files.len(), 102);
    assert!(files.contains(&"old/name.tf".to_string()));
    assert!(files.contains(&"new/name.tf".to_string()));
    assert_eq!(files.last().unwrap(), "last.tf");
}

#[tokio::test]
async fn retries_after_rate_limit() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls/1"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls/1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "head": {"ref": "feat", "sha": "abc"},
            "base": {"ref": "main", "sha": "def"},
            "merged": false, "state": "open", "draft": false,
            "merge_commit_sha": "test-merge",
            "mergeable": true, "mergeable_state": "clean"
        })))
        .mount(&server)
        .await;
    let pr = client(&server).await.get_pr(1).await.unwrap();
    assert_eq!(pr.head_ref, "feat");
    assert_eq!(pr.merge_commit_sha.as_deref(), Some("test-merge"));
    assert_eq!(pr.mergeable_state.as_deref(), Some("clean"));
    assert!(!pr.is_closed());
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn non_retryable_error_is_returned() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls/2"))
        .respond_with(ResponseTemplate::new(404).set_body_string("{\"message\":\"Not Found\"}"))
        .mount(&server)
        .await;
    let err = client(&server).await.get_pr(2).await.unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("404"), "{}", msg);
    assert!(msg.contains("/repos/o/r/pulls/2"), "{}", msg);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn comments_create_edit_react_and_status() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/repos/o/r/issues/3/comments"))
        .and(body_partial_json(json!({"body": "hello"})))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_json(json!({"id": 55, "body": "hello", "html_url": "u"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/repos/o/r/issues/comments/55"))
        .and(body_partial_json(json!({"body": "edited"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 55})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/o/r/issues/comments/55/reactions"))
        .and(body_partial_json(json!({"content": "eyes"})))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": 1})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/o/r/statuses/abc"))
        .and(body_partial_json(
            json!({"state": "pending", "context": "digger/plan", "target_url": "http://run"}),
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let c = client(&server).await;
    let created = c.create_comment(3, "hello").await.unwrap();
    assert_eq!(created.id, 55);
    c.edit_comment(55, "edited").await.unwrap();
    c.add_reaction(55, "eyes").await.unwrap();
    c.set_status(
        "abc",
        "pending",
        "digger/plan",
        "running",
        Some("http://run"),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn list_open_issues_filters_pull_requests() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/issues"))
        .and(query_param("state", "open"))
        .and(query_param("labels", "digger"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"number": 1, "title": "Drift detected in project: a", "html_url": "x"},
            {"number": 2, "title": "a PR", "html_url": "y", "pull_request": {"url": "z"}}
        ])))
        .mount(&server)
        .await;
    let issues = client(&server)
        .await
        .list_open_issues(Some("digger"))
        .await
        .unwrap();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].number, 1);
}

#[tokio::test]
async fn combined_status_and_merge() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/commits/abc/status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "state": "success",
            "statuses": [{"context": "ci/test", "state": "success"}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/repos/o/r/pulls/9/merge"))
        // Exact body match: no commit_title must be sent, so GitHub builds the
        // default merge/squash message from the PR title.
        .and(body_json(json!({"merge_method": "squash", "sha": "abc"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"merged": true})))
        .expect(1)
        .mount(&server)
        .await;
    let c = client(&server).await;
    let s = c.combined_status("abc").await.unwrap();
    assert_eq!(s.state, "success");
    assert_eq!(s.statuses[0].context, "ci/test");
    c.merge_pr(9, "squash", "abc").await.unwrap();
}

#[tokio::test]
async fn get_pr_reads_author() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls/9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "head": {"ref": "feat", "sha": "abc"},
            "base": {"ref": "main", "sha": "def"},
            "user": {"login": "alice"},
            "merged": false, "state": "open", "draft": false,
            "mergeable": true, "mergeable_state": "clean"
        })))
        .mount(&server)
        .await;
    let pr = client(&server).await.get_pr(9).await.unwrap();
    assert_eq!(pr.author, "alice");
}

#[tokio::test]
async fn list_reviews_keeps_order_and_states() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls/9/reviews"))
        .and(query_param("page", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"user": {"login": "bob"}, "state": "CHANGES_REQUESTED"},
            {"user": {"login": "carol"}, "state": "COMMENTED"},
            {"user": {"login": "bob"}, "state": "APPROVED"}
        ])))
        .mount(&server)
        .await;
    let reviews = client(&server).await.list_reviews(9).await.unwrap();
    let got: Vec<(&str, &str)> = reviews
        .iter()
        .map(|r| (r.user.as_str(), r.state.as_str()))
        .collect();
    assert_eq!(
        got,
        vec![
            ("bob", "CHANGES_REQUESTED"),
            ("carol", "COMMENTED"),
            ("bob", "APPROVED")
        ]
    );
}

#[tokio::test]
async fn compare_reads_ahead_and_behind() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/compare/main...abc123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "diverged", "ahead_by": 4, "behind_by": 2
        })))
        .mount(&server)
        .await;
    let cmp = client(&server)
        .await
        .compare("main", "abc123")
        .await
        .unwrap();
    assert_eq!(cmp.ahead_by, 4);
    assert_eq!(cmp.behind_by, 2);
}

#[tokio::test]
async fn collaborator_role_prefers_role_name_and_maps_404_to_none() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/collaborators/alice/permission"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "permission": "write", "role_name": "maintain",
            "user": {"login": "alice"}
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/collaborators/bob/permission"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "permission": "read", "user": {"login": "bob"}
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/collaborators/nobody/permission"))
        .respond_with(ResponseTemplate::new(404).set_body_string("{\"message\":\"Not Found\"}"))
        .mount(&server)
        .await;
    let c = client(&server).await;
    assert_eq!(c.collaborator_role("alice").await.unwrap(), "maintain");
    assert_eq!(c.collaborator_role("bob").await.unwrap(), "read");
    assert_eq!(c.collaborator_role("nobody").await.unwrap(), "none");
}

#[tokio::test]
async fn get_file_returns_raw_text_or_none() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/contents/dir/nikama.yml"))
        .and(query_param("ref", "release/1.0"))
        .and(header("accept", "application/vnd.github.raw+json"))
        .respond_with(ResponseTemplate::new(200).set_body_string("projects: []\n"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/contents/missing.yml"))
        .respond_with(ResponseTemplate::new(404).set_body_string("{\"message\":\"Not Found\"}"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"default_branch": "trunk"})))
        .mount(&server)
        .await;
    let c = client(&server).await;
    assert_eq!(
        c.get_file("dir/nikama.yml", "release/1.0").await.unwrap(),
        Some("projects: []\n".to_string())
    );
    assert_eq!(c.get_file("missing.yml", "main").await.unwrap(), None);
    assert_eq!(c.default_branch().await.unwrap(), "trunk");
}

#[tokio::test]
async fn merge_requirements_reads_graphql_rollup() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_partial_json(json!({"variables": {"owner": "o", "repo": "r", "number": 9}})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {"repository": {"pullRequest": {
                "reviewDecision": "REVIEW_REQUIRED",
                "commits": {"nodes": [{"commit": {"statusCheckRollup": {"contexts": {"nodes": [
                    {"__typename": "CheckRun", "name": "build", "status": "COMPLETED", "conclusion": "FAILURE",
                     "detailsUrl": "https://github.com/o/r/actions/runs/1/job/2", "isRequired": true},
                    {"__typename": "CheckRun", "name": "nikama", "status": "IN_PROGRESS", "conclusion": null,
                     "detailsUrl": "https://github.com/o/r/actions/runs/7/job/8", "isRequired": true},
                    {"__typename": "StatusContext", "context": "digger/apply", "state": "PENDING",
                     "targetUrl": "http://run", "isRequired": true},
                    {"__typename": "StatusContext", "context": "ci/opt", "state": "SUCCESS",
                     "targetUrl": null, "isRequired": false}
                ]}}}}]}
            }}}
        })))
        .mount(&server)
        .await;
    let req = client(&server).await.merge_requirements(9).await.unwrap();
    assert_eq!(req.review_decision.as_deref(), Some("REVIEW_REQUIRED"));
    let got: Vec<(&str, CheckKind, bool, &str)> = req
        .contexts
        .iter()
        .map(|c| (c.name.as_str(), c.kind, c.required, c.state.as_str()))
        .collect();
    assert_eq!(
        got,
        vec![
            ("build", CheckKind::CheckRun, true, "failure"),
            ("nikama", CheckKind::CheckRun, true, "pending"),
            ("digger/apply", CheckKind::Status, true, "pending"),
            ("ci/opt", CheckKind::Status, false, "success"),
        ]
    );
    assert_eq!(
        req.contexts[1].details_url,
        "https://github.com/o/r/actions/runs/7/job/8"
    );
}

#[tokio::test]
async fn merge_requirements_surfaces_graphql_errors() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {"repository": null},
            "errors": [{"message": "Could not resolve to a Repository"}]
        })))
        .mount(&server)
        .await;
    let err = client(&server)
        .await
        .merge_requirements(1)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("Could not resolve"), "{}", err);
}
