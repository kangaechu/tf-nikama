//! GitHub REST client tests against a local mock server.

use super::client::GithubClient;
use serde_json::json;
use wiremock::matchers::{body_partial_json, header, method, path, query_param};
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
            "mergeable": true, "mergeable_state": "clean"
        })))
        .mount(&server)
        .await;
    let pr = client(&server).await.get_pr(1).await.unwrap();
    assert_eq!(pr.head_ref, "feat");
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
        .and(body_partial_json(
            json!({"merge_method": "squash", "sha": "abc"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"merged": true})))
        .expect(1)
        .mount(&server)
        .await;
    let c = client(&server).await;
    let s = c.combined_status("abc").await.unwrap();
    assert_eq!(s.state, "success");
    assert_eq!(s.statuses[0].context, "ci/test");
    c.merge_pr(9, "squash", "abc", "auto-merge").await.unwrap();
}
