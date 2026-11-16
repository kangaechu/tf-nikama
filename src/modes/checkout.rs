//! Bind PR execution to the checkout rather than trusting an API SHA alone.

use crate::github::PrInfo;
use std::path::Path;

async fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let output = tokio::process::Command::new("git")
        .arg("--no-replace-objects")
        .args(args)
        .current_dir(root)
        .output()
        .await
        .map_err(|e| format!("could not inspect checkout: {}", e))?;
    if !output.status.success() {
        return Err(format!(
            "could not inspect checkout: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Return the PR head verified against the local checkout. A merge is only
/// accepted when GitHub identifies it as this PR's merge commit, not merely
/// because it happens to contain the expected head as an ancestor.
pub(super) async fn verified_head(root: &Path, pr: &PrInfo) -> Result<String, String> {
    let head = git(root, &["rev-parse", "--verify", "HEAD^{commit}"]).await?;
    if head == pr.head_sha {
        return Ok(head);
    }
    if pr.merge_commit_sha.as_deref() == Some(&head) {
        // A squash/rebase merge need not retain the PR head as a parent.
        // After merging, GitHub's final merge SHA establishes the association.
        if pr.merged {
            return Ok(pr.head_sha.clone());
        }
        // Read the object itself: rev-list/show may hide parents at a shallow
        // boundary, and HEAD^2 requires the parent object to be available.
        let commit = git(root, &["cat-file", "commit", &head]).await?;
        let parents: Vec<_> = commit
            .lines()
            .take_while(|line| !line.is_empty())
            .filter_map(|line| line.strip_prefix("parent "))
            .collect();
        if parents.len() == 2 && parents[1] == pr.head_sha {
            return Ok(parents[1].to_string());
        }
    }
    Err(format!(
        "checkout {} does not match pull request head {} (or its verified merge commit)",
        head, pr.head_sha
    ))
}

pub(super) async fn verify_snapshot(
    root: &Path,
    trigger_head: &str,
    pr: &PrInfo,
    wakeup_word: &str,
) -> Result<String, String> {
    let result = if trigger_head != pr.head_sha {
        Err("pull request head changed while selecting jobs".to_string())
    } else {
        verified_head(root, pr).await
    };
    result.map_err(|reason| {
        format!(
            "{}; rerun the workflow with a fresh checkout and run `{} plan` before applying",
            reason, wakeup_word
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(root: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}: {:?}", args, output);
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    struct Fixture {
        root: tempfile::TempDir,
        base: String,
        head: String,
        merge: String,
        pr: PrInfo,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            git(root.path(), &["init", "-b", "main"]);
            git(root.path(), &["commit", "--allow-empty", "-m", "base"]);
            let base = git(root.path(), &["rev-parse", "HEAD"]);
            git(root.path(), &["checkout", "-b", "feature"]);
            git(root.path(), &["commit", "--allow-empty", "-m", "head A"]);
            let head = git(root.path(), &["rev-parse", "HEAD"]);
            let tree = git(root.path(), &["rev-parse", "HEAD^{tree}"]);
            let merge = git(
                root.path(),
                &[
                    "commit-tree",
                    &tree,
                    "-p",
                    &base,
                    "-p",
                    &head,
                    "-m",
                    "test merge",
                ],
            );
            let pr = PrInfo {
                number: 1,
                head_ref: "feature".into(),
                head_sha: head.clone(),
                merge_commit_sha: Some(merge.clone()),
                base_ref: "main".into(),
                merged: false,
                state: "open".into(),
                draft: false,
                mergeable: Some(true),
                mergeable_state: Some("clean".into()),
                author: "author".into(),
                labels: vec![],
            };
            Self {
                root,
                base,
                head,
                merge,
                pr,
            }
        }

        fn push_b(&mut self) {
            git(self.root.path(), &["checkout", "feature"]);
            git(
                self.root.path(),
                &["commit", "--allow-empty", "-m", "head B"],
            );
            self.pr.head_sha = git(self.root.path(), &["rev-parse", "HEAD"]);
        }
    }

    #[tokio::test]
    async fn accepts_exact_head_and_verified_merge() {
        let f = Fixture::new();
        assert_eq!(verified_head(f.root.path(), &f.pr).await.unwrap(), f.head);
        git(f.root.path(), &["checkout", "--detach", &f.merge]);
        assert_eq!(verified_head(f.root.path(), &f.pr).await.unwrap(), f.head);
    }

    #[tokio::test]
    async fn accepts_depth_one_merge_without_parent_objects() {
        let f = Fixture::new();
        git(f.root.path(), &["checkout", "-B", "merge", &f.merge]);
        let clone = tempfile::tempdir().unwrap();
        git(
            clone.path(),
            &[
                "clone",
                "--depth=1",
                "--branch=merge",
                &format!("file://{}", f.root.path().display()),
                ".",
            ],
        );
        assert_eq!(
            git(clone.path(), &["rev-parse", "--is-shallow-repository"]),
            "true"
        );
        let parent_available = std::process::Command::new("git")
            .args(["cat-file", "-e", &f.head])
            .current_dir(clone.path())
            .output()
            .unwrap()
            .status
            .success();
        assert!(!parent_available);
        assert_eq!(verified_head(clone.path(), &f.pr).await.unwrap(), f.head);
    }

    #[tokio::test]
    async fn rejects_old_head_and_merge_after_push_even_with_stale_merge_sha() {
        let mut f = Fixture::new();
        f.push_b();
        for checkout in [&f.head, &f.merge, &f.base] {
            git(f.root.path(), &["checkout", "--detach", checkout]);
            assert!(verified_head(f.root.path(), &f.pr).await.is_err());
        }
    }

    #[tokio::test]
    async fn rejects_unrecognized_merge_and_missing_merge_sha() {
        let mut f = Fixture::new();
        git(f.root.path(), &["checkout", "--detach", &f.merge]);
        for sha in [None, Some(f.base.clone())] {
            f.pr.merge_commit_sha = sha;
            assert!(verified_head(f.root.path(), &f.pr).await.is_err());
        }
    }

    #[tokio::test]
    async fn accepts_final_squash_merge_but_not_unrelated_checkout() {
        let mut f = Fixture::new();
        f.pr.merged = true;
        // A final squash commit can have a single parent and a different SHA.
        git(f.root.path(), &["checkout", "--detach", &f.base]);
        git(f.root.path(), &["commit", "--allow-empty", "-m", "squash"]);
        f.pr.merge_commit_sha = Some(git(f.root.path(), &["rev-parse", "HEAD"]));
        assert_eq!(verified_head(f.root.path(), &f.pr).await.unwrap(), f.head);
        git(f.root.path(), &["checkout", "--detach", &f.base]);
        assert!(verified_head(f.root.path(), &f.pr).await.is_err());
    }

    #[tokio::test]
    async fn rejects_job_selection_from_an_older_pr_snapshot() {
        let mut f = Fixture::new();
        f.push_b();
        let reason = verify_snapshot(f.root.path(), &f.head, &f.pr, "nikama")
            .await
            .unwrap_err();
        assert!(reason.contains("head changed while selecting jobs"));
        assert!(reason.contains("fresh checkout"));
        assert!(reason.contains("`nikama plan`"));
    }

    #[tokio::test]
    async fn rejects_missing_git_checkout_with_retry_guidance() {
        let f = Fixture::new();
        let empty = tempfile::tempdir().unwrap();
        let reason = verify_snapshot(empty.path(), &f.head, &f.pr, "digger")
            .await
            .unwrap_err();
        assert!(reason.contains("could not inspect checkout"));
        assert!(reason.contains("`digger plan`"));
    }

    #[tokio::test]
    async fn pr_run_refuses_to_plan_or_apply_a_checkout_mislabelled_as_new_head() {
        use crate::cli::Env;
        use crate::config::load_from_str;
        use crate::github::{GithubClient, GithubContext};
        use serde_json::json;
        use std::sync::Arc;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mut f = Fixture::new();
        f.push_b();
        // Checkout A is left behind while the API already reports B. Test
        // both an event carrying A and one already carrying B.
        git(f.root.path(), &["checkout", "--detach", &f.head]);
        for (command, event_head) in [
            ("plan", &f.head),
            ("plan", &f.pr.head_sha),
            ("apply", &f.pr.head_sha),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/repos/o/r/pulls/1"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "head": {"ref": "feature", "sha": f.pr.head_sha},
                    "base": {"ref": "main"},
                    "merge_commit_sha": f.merge,
                    "state": "open", "merged": false,
                    "mergeable": true, "mergeable_state": "clean"
                })))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/repos/o/r/pulls/1/files"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                    {"filename": "infra/main.tf"}
                ])))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/repos/o/r/issues/1/comments"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": 1})))
                .mount(&server)
                .await;
            Mock::given(method("PATCH"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 1})))
                .mount(&server)
                .await;
            let config = Arc::new(load_from_str(&format!(
                "projects:\n  - name: infra\n    dir: infra\n    workflow: w\nworkflows:\n  w:\n    workflow_configuration:\n      on_pull_request_pushed: [digger {}]\n",
                command
            ), f.root.path()).unwrap());
            let ctx = GithubContext::parse(
                &json!({
                    "event_name": "pull_request", "repository": "o/r",
                    "event": {
                        "action": "synchronize",
                        "pull_request": {
                            "number": 1,
                            "head": {"ref": "feature", "sha": event_head},
                            "base": {"ref": "main", "sha": f.base}
                        },
                        "repository": {"full_name": "o/r", "default_branch": "main"},
                        "sender": {"login": "author"}
                    }
                })
                .to_string(),
            )
            .unwrap();
            let env = Env {
                repo_root: f.root.path().to_path_buf(),
                work_root: f.root.path().join("work"),
                max_parallel: None,
                serialize_init: false,
                cli_config: None,
            };
            let github = GithubClient::new("token", "o", "r", Some(&server.uri()));
            assert_eq!(
                crate::modes::pr_event::run(&env, config, &ctx, github).await,
                crate::modes::exit::JOB_FAILED
            );
            assert!(
                !env.work_root.exists(),
                "no Terraform work or plan was created"
            );
            let requests = server.received_requests().await.unwrap();
            let bodies: Vec<_> = requests
                .iter()
                .filter_map(|r| r.body_json::<serde_json::Value>().ok())
                .collect();
            assert!(bodies.iter().any(|body| body["state"] == "failure"));
            assert!(bodies.iter().any(|body| body["body"]
                .as_str()
                .is_some_and(|s| s.contains("fresh checkout"))));
        }
    }
}
