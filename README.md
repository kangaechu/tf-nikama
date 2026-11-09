# tf-nikama

Backendless Terraform / OpenTofu automation for GitHub pull requests, as a single
small binary that runs inside GitHub Actions.

`nikama` is a Rust re-implementation of the *backendless mode* of
[Digger](https://github.com/diggerhq/digger) (`no-backend: true`). It keeps the
parts that work without an orchestrator backend, drops everything else, and
fixes the pain points of the original:

| | Digger backendless | tf-nikama |
|---|---|---|
| Binary | ~115 MB Go binary with 3 cloud SDKs, OPA, terragrunt | small Rust binary, AWS SDK only |
| Plans | strictly sequential | parallel per project (`terraform init` serialised for plugin-cache safety) |
| PR comments | several comments per project, no size handling | one comment per run, updated in place, split at the 65,536-char limit |
| Config | `digger.yml` | `nikama.yml` (same schema; unsupported keys fail fast) |

## Scope

Supported in the first release:

- **GitHub** `pull_request` and `issue_comment` events, `workflow_dispatch` (manual) and `schedule` (drift)
- **Terraform** and **OpenTofu**
- **AWS**: DynamoDB PR locks (schema-compatible with Digger), S3 plan storage,
  GitHub OIDC → STS role assumption, Cognito Identity
- Comment commands: `digger plan|apply|lock|unlock|help` (also `nikama ...`), with `-p/--project` and `-d/--directory`
- Commit statuses `digger/plan` and `digger/apply`, optional auto-merge
- Drift detection with Slack webhook or GitHub Issue notifications

Not supported (and rejected at config load): Terragrunt, Pulumi, GCP/Azure
lock or plan storage, GitHub Artifacts plan storage, OPA policies, Digger
backend features.

## Quick start

`.github/workflows/nikama.yml`:

```yaml
name: nikama
on:
  pull_request:
    types: [opened, synchronize, reopened, closed, ready_for_review]
  issue_comment:
    types: [created]

permissions:
  contents: read
  pull-requests: write
  statuses: write
  id-token: write

concurrency:
  group: nikama-${{ github.event.pull_request.number || github.event.issue.number }}
  cancel-in-progress: false

jobs:
  nikama:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          ref: ${{ github.event_name == 'issue_comment' && format('refs/pull/{0}/merge', github.event.issue.number) || '' }}
      - uses: hashicorp/setup-terraform@v3
        with:
          terraform_wrapper: false
      - uses: aws-actions/configure-aws-credentials@v4
        with:
          role-to-assume: arn:aws:iam::123456789012:role/nikama
          aws-region: ap-northeast-1
      - uses: kangaechu/tf-nikama@v0.1.0
        with:
          upload-plan-destination: aws
          upload-plan-destination-s3-bucket: my-plan-bucket
```

`nikama.yml` at the repository root:

```yaml
projects:
  - name: network
    dir: infra/network
  - name: app
    dir: infra/app
    depends_on: [network]

concurrency:
  max_parallel: 4
```

See [`examples/nikama.yml`](examples/nikama.yml) for the full schema and
[`docs/DESIGN.md`](docs/DESIGN.md) for the design.

## How a run works

1. The event is mapped to commands using each project's
   `workflow_configuration` (defaults: push → `digger plan`, close / merge → `digger unlock`).
2. Impacted projects are computed from the PR's changed files
   (`dir/**`, `include_patterns`, `exclude_patterns`, plus downstream
   `depends_on` projects in `hard` mode).
3. One run comment is posted immediately with every project as *pending*, and
   the commit status is set to *pending*.
4. PR locks are acquired for all projects up front; projects locked by another
   open PR are reported and skipped.
5. Plans run in parallel (`concurrency.max_parallel`); applies run level by
   level in dependency order. The comment is edited as projects finish.
6. The commit status is set to success / failure; `auto_merge` merges the PR
   when every apply succeeded.

## Configuration file

`nikama.yml` (or `nikama.yaml`). A `digger.yml` is still read as a fallback
with a warning. Unknown keys and keys for unsupported features are errors.

Top-level keys: `projects`, `workflows`, `generate_projects`,
`dependency_configuration`, `traverse_to_nested_projects`, `auto_merge`,
`auto_merge_strategy`, `pr_locks`, `allow_draft_prs`,
`drift_include_patterns`, `drift_exclude_patterns`, `concurrency`, `comment`
(plus `git_timeout` and `telemetry`, accepted for compatibility).

## Environment variables

The action sets these for you; when running the binary directly:

| Variable | Purpose |
|---|---|
| `GITHUB_TOKEN`, `GITHUB_CONTEXT`, `GITHUB_REPOSITORY` | required for PR mode |
| `INPUT_DIGGER_MODE` | `` / `manual` / `drift-detection` |
| `INPUT_DIGGER_COMMAND`, `INPUT_DIGGER_PROJECT` | manual mode |
| `LOCK_PROVIDER` (`aws`/`noop`), `NIKAMA_LOCK_TABLE`, `DISABLE_LOCKING` | PR locks |
| `PLAN_UPLOAD_DESTINATION` (`aws`), `AWS_S3_BUCKET`, `PLAN_UPLOAD_S3_*` | plan storage |
| `REPORTING_STRATEGY` | `comments_per_run` / `latest_run_comment` / `per_project` |
| `INPUT_DRIFT_DETECTION_SLACK_NOTIFICATION_URL`, `INPUT_DRIFT_GITHUB_ISSUES` | drift notifications |
| `NIKAMA_CONFIG`, `NIKAMA_MAX_PARALLEL`, `NIKAMA_LOG_LEVEL` | CLI options |
| `TF_PLUGIN_CACHE_DIR` | when set, `init` steps run one at a time |

Validate a configuration locally:

```sh
nikama --validate --local
```

## Exit codes

`0` ok / nothing to do, `1` configuration error, `2` lock provider error,
`3` context parse error, `4` initialisation error, `6` event error,
`7` command error, `8` at least one job failed, `10` not in GitHub Actions.

## Acknowledgements

tf-nikama is an independent re-implementation of the backendless mode of
[Digger](https://github.com/diggerhq/digger) by Digger HQ, licensed under
Apache-2.0. Configuration keys, comment commands, the DynamoDB lock schema and
the S3 plan key format are kept compatible so existing setups can migrate.

## License

Apache License 2.0. See [LICENSE](LICENSE).
