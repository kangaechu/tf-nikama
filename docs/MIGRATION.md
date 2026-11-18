# Migrating from Digger (backendless mode)

[日本語版](MIGRATION.ja.md)

This guide is for repositories that run Digger with `no-backend: true` and
want to switch to tf-nikama. If you use the Digger backend / orchestrator,
Terragrunt, Pulumi or Azure, tf-nikama is not a drop-in replacement; see
[What is not supported](#what-is-not-supported) first.

The short version:

1. Rename `digger.yml` to `nikama.yml` and remove unsupported keys.
2. Replace the `diggerhq/digger` step in your workflow with a Terraform setup
   step plus `kangaechu/tf-nikama`.
3. Keep your DynamoDB lock table, S3 plan bucket (or the Cloud Storage
   buckets on GCP), branch protection rules and PR comment habits. They all
   carry over unchanged.

## Compatibility at a glance

| Area | Carries over? | Notes |
|---|---|---|
| `digger.yml` keys and defaults | Yes | Same schema. Unsupported keys are rejected at load time instead of being ignored. |
| DynamoDB lock table | Yes | Same table name (`DiggerDynamoDBLockTable`), same key schema. Locks held by open PRs stay valid. |
| S3 plan storage | Yes | Same bucket variables and the same object key format `<owner>-<repo>-<pr>-<project>.tfplan`. nikama additionally records the PR head commit in the object metadata (`x-amz-meta-head-sha`) and refuses to apply a plan from another commit; plans written by Digger have no metadata and are applied with a warning. |
| GCS locks and plan storage | Yes | Same `GOOGLE_STORAGE_LOCK_BUCKET` / `GOOGLE_STORAGE_PLAN_ARTEFACT_BUCKET` variables, same object names and the same `LockId` metadata key. Lock objects are now created with a precondition, so two runs can no longer both acquire a lock. Plan objects get a `head-sha` metadata entry with the same commit check as on S3. |
| PR comment commands | Yes | `digger plan|apply|lock|unlock|help` keep working. `nikama ...` is accepted as well, and `wakeup_word` can replace both. |
| Commit status contexts | Yes | Still `digger/plan` and `digger/apply`, so branch protection rules need no change. |
| Environment variables | Yes | `INPUT_DIGGER_*`, `LOCK_PROVIDER`, `PLAN_UPLOAD_*`, `REPORTING_STRATEGY`, `DIGGER_FILENAME`, `DIGGER_GITHUB_TOKEN` are read as before. |
| `DIGGER_PR_NUMBER` | Yes | Still exported via `GITHUB_ENV`; also available as the `pr_number` step output. |
| Workflow file (`.github/workflows/*.yml`) | No | The action has different inputs and does not install Terraform or AWS credentials for you. See below. |
| PR comment layout | No | One comment per run, updated in place. Existing Digger comments on open PRs are left untouched. |

## Step 1: configuration file

Rename the file:

```sh
git mv digger.yml nikama.yml
```

tf-nikama still reads `digger.yml` as a fallback and logs a warning, so the
rename can happen in a later PR if you prefer. The fallback is scheduled for
removal in v1.0.

Then validate it. Unlike Digger, tf-nikama fails fast on any key it does not
understand:

```sh
nikama --validate --local
```

Keys that cause an error, grouped by where they appear:

| Location | Keys |
|---|---|
| top level | `respect_layers`, `delete_prior_comments`, `disable_digger_apply_comment`, `disable_digger_apply_status_check`, `comment_render_mode`, `trusted_appIDs`, `report_terraform_outputs`, `reporting`, `apply_after_merge`, `mention_drifted_projects_in_pr` |
| `projects[]` | `terragrunt`, `pulumi`, `pulumi_stack`, `workflow_file`, `generated`, `drift_terragrunt_parallelism` |
| `generate_projects` | `terragrunt`, `terragrunt_parsing`, `drift_terragrunt_parallelism` |
| `generate_projects.blocks[]` | `terragrunt`, `root_dir`, `terragrunt_parsing`, `workflow_file`, `drift_terragrunt_parallelism` |

Most of these are backend-only settings that had no effect in backendless
mode, so deleting them changes nothing. `git_timeout` and `telemetry` are
accepted and ignored.

Two behavioural differences in the config are worth knowing:

- **`workflow_configuration` is filled in with defaults.** In Digger, writing
  the block made all three `on_*` keys mandatory. tf-nikama completes missing
  keys with the defaults (`nikama plan` / `nikama unlock` / `nikama unlock`;
  `digger ...` entries keep working).
- **A missing config file is an error.** Digger continued with an empty
  configuration; tf-nikama exits with code 1.

Optional additions that have no Digger equivalent:

```yaml
concurrency:
  max_parallel: 4        # parallel plans (default 4)
  apply_parallelism: 1   # applies within one dependency level (default 1)

comment:
  mode: single           # single (default) | per_project | latest
  show_empty_plans: table

wakeup_word: [nikama, digger]  # word(s) a comment command starts with (default shown)
autoplan_modules: false        # nikama-only: follow local `module` sources when computing impacted projects
allowed_plan_args: []          # nikama-only: arguments a comment may pass after `--` (empty: refused)
dependency_configuration:
  on_failure: skip_dependents  # nikama-only: skip_dependents (default) | abort | continue after a failed apply
```

See [`examples/nikama.yml`](../examples/nikama.yml) for the full schema.

## Step 2: workflow file

A typical Digger backendless workflow looks like this:

```yaml
jobs:
  digger-job:
    runs-on: ubuntu-latest
    permissions:
      contents: write
      actions: write
      id-token: write
      issues: write
      pull-requests: write
      statuses: write
    steps:
      - uses: actions/checkout@v7
      - uses: aws-actions/configure-aws-credentials@v6
        with:
          role-to-assume: arn:aws:iam::123456789012:role/digger
          aws-region: ap-northeast-1
      - uses: diggerhq/digger@vLatest
        with:
          no-backend: true
          setup-terraform: true
          terraform-version: 1.9.0
          configure-checkout: true
          cache-dependencies: true
          upload-plan-destination: aws
          upload-plan-destination-s3-bucket: my-plan-bucket
          reporting-strategy: comments_per_run
        env:
          GITHUB_CONTEXT: ${{ toJson(github) }}
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
```

The equivalent tf-nikama workflow:

```yaml
jobs:
  nikama:
    runs-on: ubuntu-latest
    permissions:
      contents: read        # write only if auto_merge: true
      pull-requests: write
      statuses: write
      id-token: write
    steps:
      - uses: actions/checkout@v7
        with:
          ref: ${{ github.event_name == 'issue_comment' && format('refs/pull/{0}/merge', github.event.issue.number) || '' }}
      - uses: hashicorp/setup-terraform@v4
        with:
          terraform_version: 1.9.0
          terraform_wrapper: false
      - uses: aws-actions/configure-aws-credentials@v6
        with:
          role-to-assume: arn:aws:iam::123456789012:role/digger
          aws-region: ap-northeast-1
      - uses: kangaechu/tf-nikama@v0.8.0
        with:
          upload-plan-destination: aws
          upload-plan-destination-s3-bucket: my-plan-bucket
```

What changed and why:

- **Terraform / OpenTofu is not installed by the action.** Digger's
  `setup-terraform`, `terraform-version`, `setup-opentofu` and
  `opentofu-version` inputs are gone. Add `hashicorp/setup-terraform` or
  `opentofu/setup-opentofu` yourself. Disable the wrapper (`terraform_wrapper:
  false`) so tf-nikama can read the real exit code.
- **Checkout of the PR head on comments is explicit.** Digger did this behind
  `configure-checkout: true`. tf-nikama relies on the `ref:` expression on
  `actions/checkout` shown above.
- **Cloud credentials come from your workflow.** Digger's `setup-aws`,
  `aws-role-to-assume`, `aws-access-key-id` and friends are gone. Keep using
  `aws-actions/configure-aws-credentials`. Per-project `aws_role_to_assume`
  and `aws_cognito_oidc` in `nikama.yml` still work.
  On Google Cloud, replace `setup-google-cloud` and the `google-*` auth inputs
  with `google-github-actions/auth`; tf-nikama reads the credentials it writes
  (`GOOGLE_APPLICATION_CREDENTIALS`) for the lock and plan buckets.
- **`GITHUB_CONTEXT` and `GITHUB_TOKEN` are set by the action.** You no longer
  need the `env:` block. Pass a different token with the `github-token` input.
- **`no-backend: true` is gone.** There is no backend mode to turn off.
- **Fewer permissions.** `actions: write` and `issues: write` are not needed
  for PR runs. Add `issues: write` only for drift detection with
  `drift-github-issues: true`, and `contents: write` only for `auto_merge`.
  If the base branch restricts who can push, `GITHUB_TOKEN` cannot merge at
  all; see "Auto-merge and branch protection" in [CONFIGURATION.md](CONFIGURATION.md)
  for the GitHub App setup.
- **Plugin cache.** `cache-dependencies: true` set `TF_PLUGIN_CACHE_DIR` and
  wired up `actions/cache`. tf-nikama does not do this. If you want a provider
  cache, set `TF_PLUGIN_CACHE_DIR` and add `actions/cache` yourself; when the
  variable is set, `init` steps run one at a time to keep the cache
  consistent (plans still run in parallel).

### Action input mapping

| Digger input | tf-nikama input | Notes |
|---|---|---|
| `no-backend` | (none) | Always backendless. |
| `setup-terraform`, `terraform-version`, `setup-opentofu`, `opentofu-version`, `setup-tfenv` | (none) | Use a setup action. |
| `setup-aws`, `aws-*` | (none) | Use `aws-actions/configure-aws-credentials`. |
| `setup-google-cloud`, `google-auth-credentials`, `google-workload-identity-provider*`, `google-service-account` | (none) | Use `google-github-actions/auth`. |
| `google-lock-bucket`, `upload-plan-destination-gcp-bucket` | same names | Unchanged. Set `lock-provider: gcp` for the lock bucket. |
| `setup-azure`, `upload-plan-destination-azure-*` | (none) | Azure not supported. |
| `setup-terragrunt`, `terragrunt-version`, `setup-checkov`, `checkov-version` | (none) | Not supported. |
| `configure-checkout` | (none) | Set `ref:` on `actions/checkout`. |
| `cache-dependencies` | (none) | Set `TF_PLUGIN_CACHE_DIR` yourself. |
| `digger-filename` | `config-file` | `DIGGER_FILENAME` env is also still honoured. |
| `digger-token`, `digger-hostname`, `digger-organisation`, `digger-spec` | (none) | Backend only; ignored if set in `env`. |
| `disable-locking` | `lock-provider: noop` | `DISABLE_LOCKING=true` env also works. |
| (none) | `lock-provider` | New. `aws` (default), `s3`, `gcp` or `noop`. Digger picked `gcp` when `google-lock-bucket` was set. `s3` keeps locks in an S3 bucket (`aws-lock-bucket`) instead of DynamoDB; it has no Digger equivalent, so switch to it only once no open PR holds a DynamoDB lock. |
| (none) | `lock-table` | New. Defaults to `DiggerDynamoDBLockTable`. |
| `upload-plan-destination` | `upload-plan-destination` | `aws`, `gcp` or empty. `github` and `azure` are not supported. |
| `upload-plan-destination-s3-*` | same names | Unchanged. |
| `reporting-strategy` | `reporting-strategy` | Same values, mapped to `comment.mode` in `nikama.yml` (which wins when written there). `comments_per_run` now means one comment per run, edited in place. |
| `mode`, `command`, `project` | `mode`, `command`, `project` | Unchanged (manual and drift modes). |
| `drift-detection-slack-notification-url` | same | Unchanged. |
| `drift-github-issues` | same | The default label is `digger`. Set `drift-github-issue-label` to override it. |
| `github-token` | `github-token` | Unchanged. |
| (none) | `max-parallel`, `log-level`, `version` | New. |
| (none) | `config-ref`, `allow-run-steps` | New. Read `nikama.yml` from a trusted ref on PR runs / refuse `run:` steps ([CONFIGURATION.md](CONFIGURATION.md) "Trusting the configuration"). |
| (none) | `allowed-providers` | New. Only the listed Terraform providers can be installed ([CONFIGURATION.md](CONFIGURATION.md) "Restricting providers"). |

### Manual and drift workflows

`workflow_dispatch` (manual) and `schedule` (drift) workflows keep the same
`mode`, `command` and `project` inputs. Replace the `diggerhq/digger` step as
above and keep the rest:

```yaml
      - uses: kangaechu/tf-nikama@v0.8.0
        with:
          mode: manual
          command: digger apply
          project: ${{ inputs.project }}
```

```yaml
      - uses: kangaechu/tf-nikama@v0.8.0
        with:
          mode: drift-detection
          drift-detection-slack-notification-url: ${{ secrets.SLACK_WEBHOOK }}
```

Drift mode requires at least one notification target and exits with code 8
otherwise.

## Step 3: switch over

1. Merge the workflow change. PRs opened after the merge use tf-nikama.
2. PRs that are already open keep their old Digger comments; the next push or
   `digger plan` comment produces a tf-nikama comment below them. Locks those
   PRs hold are honoured because the DynamoDB table is shared.
3. Stored plans from Digger runs are reused by `digger apply` because the S3
   key format is identical.

There is no state to migrate and nothing to run against the lock table or
the bucket. Rolling back is a revert of the workflow change.

## Behavioural differences

These change what you see, not what you configure.

| | Digger | tf-nikama |
|---|---|---|
| Plans | sequential | parallel per project (`concurrency.max_parallel`) |
| PR comments | several per project; no size limit handling | one per run, edited as projects finish; split at 65,536 chars |
| Commit status | set only at the end | `pending` at start, then `success` / `failure` |
| `-p` on comments | one project | repeatable; also `--project`, `-d` / `--directory` |
| Unknown project in `-p` | run fails | warned in the comment, rest runs |
| `digger help` | exits 7 when there are impacted projects | posts help, exits 0 |
| Comment on a plain issue | API error | ignored, exits 0 |
| Comment command from a user without write access | runs | refused with a comment, exits 0 (see [CONFIGURATION.md](CONFIGURATION.md) "Who can run commands") |
| `approved` requirement | any user's review counts | only reviews from users with write access count, as with branch protection |
| `ready_for_review` | ignored | runs `on_pull_request_pushed` |
| Plan without locks | not available | `digger plan --no-lock` on a comment, or the `no_lock_label` label (see [CONFIGURATION.md](CONFIGURATION.md) "PR locks") |
| Lock on apply only | not available | `pr_locks: on_apply`; `true` / `false` keep their meaning (see [CONFIGURATION.md](CONFIGURATION.md) "PR locks") |
| Terraform arguments on a comment | not available | `nikama plan -- -target=...`, limited by `allowed_plan_args` (see [CONFIGURATION.md](CONFIGURATION.md) "Extra arguments on comments") |
| Changes to a local module | project not impacted unless `include_patterns` lists the module | `autoplan_modules: true` follows `module` sources (see [CONFIGURATION.md](CONFIGURATION.md) "Local modules") |
| Project `branch` | accepted, not used for project selection | regular expression on the PR's base branch; non-matching projects are not impacted (see [CONFIGURATION.md](CONFIGURATION.md) "Base branch filter") |
| `dependency_configuration: hard` | no downstream expansion in the CLI | downstream projects are planned too |
| Replace in plan summary | counted as create or delete | own column (`±`) |
| `destroy` failure (manual mode) | reported as success | reported as failure |
| `mergeable_state: blocked` | inspects check runs | inspects the review decision and the *required* check runs and commit statuses other than its own (`digger/*`), ignoring jobs of the current workflow run that are still running |
| Telemetry | on by default | none |

## What is not supported

tf-nikama targets Terraform / OpenTofu on AWS and Google Cloud. These Digger
features are out of scope and rejected at startup or config load:

- Digger backend / orchestrator (`DIGGER_TOKEN`, `DIGGER_HOSTNAME`, run specs)
- Terragrunt (including `generate_projects` with `terragrunt: true`) and Pulumi
- Azure locks and plan storage
- Plan storage in GitHub Artifacts (`upload-plan-destination: github`)
- OPA / rego policies and Checkov
- GitLab, Bitbucket and Azure DevOps (GitHub Enterprise Server is not tested; `GITHUB_API_URL` is honoured)

If you depend on any of these, stay on Digger.

## Getting help

If `nikama --validate` rejects a key you believe was working in backendless
Digger, or a PR behaves differently from the table above, please
[open an issue](https://github.com/kangaechu/tf-nikama/issues/new) with the
relevant part of `nikama.yml` and the workflow log.
