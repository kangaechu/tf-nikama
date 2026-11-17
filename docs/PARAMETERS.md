# Parameter reference

[日本語版](PARAMETERS.ja.md)

Every knob in one place: the inputs of the GitHub Action, the keys of
`nikama.yml`, and the environment variables and flags of the `nikama` binary.
What each setting does in context is explained in
[CONFIGURATION.md](CONFIGURATION.md).

- [Action inputs](#action-inputs)
- [Action outputs](#action-outputs)
- [`nikama.yml`](#nikamayml)
  - [Top-level keys](#top-level-keys)
  - [`projects[]`](#projects)
  - [`workflows.<name>`](#workflowsname)
  - [`generate_projects`](#generate_projects)
  - [`aws_role_to_assume`](#aws_role_to_assume)
  - [`aws_cognito_oidc`](#aws_cognito_oidc)
  - [Keys accepted and ignored](#keys-accepted-and-ignored)
  - [Keys rejected](#keys-rejected)
- [Binary: flags and environment variables](#binary-flags-and-environment-variables)
- [Variables available to `run:` steps](#variables-available-to-run-steps)

## Action inputs

All inputs of `kangaechu/tf-nikama` are optional.

### General

| Input | Default | Description |
|---|---|---|
| `version` | the action ref | Release tag to install (for example `v0.7.0`). A non-release ref builds from source with cargo. |
| `mode` | `` | Empty for pull request events, `manual` for `workflow_dispatch`, `drift-detection` for scheduled drift runs. |
| `command` | `` | Manual mode only: `digger plan`, `digger apply` or `digger destroy`. |
| `project` | `` | Manual mode only: the project to run. |
| `github-token` | `${{ github.token }}` | Token for every GitHub API call. Pass a GitHub App or PAT token to merge onto a branch that restricts who can push. |
| `max-parallel` | `` | Maximum number of concurrent plans; overrides `concurrency.max_parallel`. |
| `log-level` | `INFO` | `ERROR`, `WARN`, `INFO` or `DEBUG`. |

### Configuration and trust

| Input | Default | Description |
|---|---|---|
| `config-file` | `` | Alternative configuration file path. Default lookup: `nikama.yml`, `nikama.yaml`, then `digger.yml` (with a warning). |
| `config-ref` | `` | Pull request runs only: read the configuration from this git ref through the GitHub API instead of the checkout. `default` means the repository's default branch. |
| `allow-run-steps` | `true` | `false` refuses any configuration that contains a `run:` step. |
| `allowed-providers` | `` | Comma-separated provider source patterns (`hashicorp/aws, hashicorp/*`). When set, `init` refuses every other provider. |

### PR locks

| Input | Default | Description |
|---|---|---|
| `lock-provider` | `aws` | `aws` (DynamoDB), `s3` (S3 bucket), `gcp` (Cloud Storage bucket) or `noop` (no locks). |
| `lock-table` | `DiggerDynamoDBLockTable` | DynamoDB table for `lock-provider: aws`. Created on first use when the role may `dynamodb:CreateTable`. |
| `aws-lock-bucket` | `` | S3 bucket for `lock-provider: s3`. |
| `google-lock-bucket` | `` | Cloud Storage bucket for `lock-provider: gcp`. |

### Plan storage

| Input | Default | Description |
|---|---|---|
| `upload-plan-destination` | `` | `aws` (S3), `gcp` (Cloud Storage) or empty to apply from a plan produced just before the apply. |
| `upload-plan-destination-s3-bucket` | `` | S3 bucket for plan files. |
| `upload-plan-destination-s3-encryption-enabled` | `false` | Server-side encryption for stored plans. |
| `upload-plan-destination-s3-encryption-type` | `AES256` | `AES256` or `KMS`. |
| `upload-plan-destination-s3-encryption-kms-key-id` | `` | KMS key id when the type is `KMS`. |
| `upload-plan-destination-gcp-bucket` | `` | Cloud Storage bucket for plan files. |

### Reporting and drift detection

| Input | Default | Description |
|---|---|---|
| `reporting-strategy` | `comments_per_run` | `comments_per_run` (one comment per run, edited in place), `latest_run_comment` or `per_project`. |
| `drift-detection-slack-notification-url` | `` | Slack incoming webhook for drift notifications. |
| `drift-github-issues` | `false` | Create or update one GitHub Issue per drifted project (needs `issues: write`). |
| `block-filters` | `` | Drift mode: comma-separated `generate_projects` block names to include. |

## Action outputs

| Output | Description |
|---|---|
| `pr_number` | Pull request number the run acted on. Also exported as `DIGGER_PR_NUMBER` through `GITHUB_ENV`. |

## `nikama.yml`

Unknown keys are errors. The file is read from the checkout unless
`config-ref` is set. A complete example is in
[`../examples/nikama.yml`](../examples/nikama.yml).

### Top-level keys

| Key | Default | Description |
|---|---|---|
| `projects` | `[]` | List of projects; see [`projects[]`](#projects). |
| `workflows` | `{default: ...}` | Named workflows; see [`workflows.<name>`](#workflowsname). The `default` workflow always exists. |
| `generate_projects` | none | Discover projects from directories; see [`generate_projects`](#generate_projects). |
| `dependency_configuration.mode` | `hard` | `hard`: a change to a project also plans its downstream `depends_on` projects. `soft`: dependencies only order applies. |
| `dependency_configuration.on_failure` | `skip_dependents` | What happens to the remaining applies when a project fails. `skip_dependents`: projects that depend on it (directly or transitively) are skipped, unrelated projects still apply. `abort`: every later dependency level is skipped. `continue`: dependents apply anyway. See "How a run works" in [CONFIGURATION.md](CONFIGURATION.md). |
| `traverse_to_nested_projects` | `true` | Whether `generate_projects` descends into a directory that is itself a project. |
| `auto_merge` | `false` | Merge the pull request when every apply succeeded. |
| `auto_merge_strategy` | `squash` | `squash`, `merge` or `rebase`. |
| `pr_locks` | `on_plan` | `on_plan` / `true`: lock on the first plan with changes (a plan without changes gives the lock back). `on_apply`: lock only on apply. `disabled` / `false`: never lock. |
| `allow_draft_prs` | `false` | `false`: draft pull requests run nothing and hold no locks. |
| `no_lock_label` | none | Pull requests carrying this label plan without taking locks. |
| `wakeup_word` | `[nikama, digger]` | Word or list of words a comment command starts with; replaces the defaults. |
| `autoplan_modules` | `false` | Treat local modules a project references (`./` or `../` sources, followed transitively) as part of the project. |
| `allowed_plan_args` | `[]` | Patterns for arguments a comment may pass to `terraform plan` after `--`. Empty: `--` is refused. |
| `allowed_providers` | `[]` | Provider source patterns this configuration allows. Only narrows the `allowed-providers` input. |
| `drift_include_patterns` | `[]` | Projects whose `dir` matches are included in drift detection. |
| `drift_exclude_patterns` | `[]` | Projects whose `dir` matches are excluded from drift detection. |
| `concurrency.max_parallel` | `4` | Concurrent plans. |
| `concurrency.apply_parallelism` | `1` | Concurrent applies within one dependency level. |
| `comment.mode` | `single` | `single`: one comment per run. `per_project`: one comment per project. `latest`: a single comment reused and overwritten by every run. |
| `comment.show_empty_plans` | `table` | Projects without changes: `table` shows their row only, `hide` drops the row, `details` also includes their plan output. |
| `comment.per_project_status` | `false` | Accepted for compatibility; not used. |

### `projects[]`

| Key | Default | Description |
|---|---|---|
| `name` | required | Project name, used with `-p` and in `depends_on`. |
| `dir` | required | Directory relative to the repository root. |
| `workspace` | `default` | Terraform workspace. |
| `workflow` | `default` | Name of the workflow in `workflows` to use. |
| `opentofu` | `false` | Run `tofu` instead of `terraform`. |
| `include_patterns` | `[]` | Extra glob patterns; a changed file matching one impacts the project. |
| `exclude_patterns` | `[]` | Glob patterns; changed files matching one do not impact the project. |
| `depends_on` | `[]` | Projects that must apply before this one. |
| `branch` | none | Regular expression matched against the pull request's base branch; non-matching PRs do not impact the project. |
| `apply_requirements` | `[mergeable]` | Any of `mergeable`, `approved`, `undiverged`; each is checked before an apply. |
| `drift_detection` | `true` | Include the project in drift runs. |
| `drift_include_patterns`, `drift_exclude_patterns` | `[]` | Project-level drift filters. |
| `aws_role_to_assume` | none | See [`aws_role_to_assume`](#aws_role_to_assume). |
| `aws_cognito_oidc` | none | See [`aws_cognito_oidc`](#aws_cognito_oidc). |
| `layer`, `alias`, `block_name` | `0`, none, none | Accepted for compatibility with `digger.yml`; not used. |

### `workflows.<name>`

A workflow may be `null` to use the built-in steps (`plan` = `init`, `plan`;
`apply` = `init`, `apply`).

| Key | Default | Description |
|---|---|---|
| `env_vars.state[]` | `[]` | Environment variables for `init` (the state backend). Each entry: `name` plus `value` or `value_from` (name of a workflow environment variable). |
| `env_vars.commands[]` | `[]` | Environment variables for `plan` / `apply` / `run` steps; same shape. Variables nikama sets itself (`TF_CLI_CONFIG_FILE` for `allowed-providers`, `TF_DATA_DIR`) win over entries with the same name. |
| `plan.steps[]` | `[init, plan]` | Steps of the plan stage; see below. |
| `plan.filter_regex`, `apply.filter_regex` | none | Regular expression; matches in the stage output are replaced with `<REDACTED>` in logs and the comment. |
| `apply.steps[]` | `[init, apply]` | Steps of the apply stage. |
| `workflow_configuration.on_pull_request_pushed` | `[digger plan]` | Commands run on push. `destroy` is refused here, as it is in comments. |
| `workflow_configuration.on_pull_request_closed` | `[digger unlock]` | Commands run on close or merge. |
| `workflow_configuration.on_commit_to_default` | `[digger unlock]` | Commands run on a commit to the default branch. |
| `workflow_configuration.on_pull_request_to_draft` | `[]` | Commands run when a pull request is converted to a draft. |
| `workflow_configuration.skip_merge_check` | `false` | Skip the `mergeable` requirement for this workflow. |

A step is one of:

```yaml
- init                              # bare name
- plan:
    extra_args: ["-var-file=env.tfvars"]
- apply
  extra_args: ["-auto-approve"]     # sibling form, same meaning
- run: "echo done >> $NIKAMA_OUT"   # shell command
  shell: bash                       # optional
```

`init`, `plan` and `apply` accept `extra_args`. A plan stage must start with
`init`. `run:` steps are refused when `allow-run-steps` is `false`.

### `generate_projects`

Use either `include` / `exclude` at this level or `blocks`, not both. A
directory counts as a project when it contains a `*.tf` or `*.tofu` file;
directories named `modules` and hidden directories are skipped.

| Key | Default | Description |
|---|---|---|
| `include` | none | Glob the directory must match. Without it nothing is generated. |
| `exclude` | none | Glob the directory must not match. |
| `blocks[]` | `[]` | Named groups, each with the keys below. |
| `aws_role_to_assume`, `aws_cognito_oidc` | none | Applied to every generated project. |
| `drift_include_patterns`, `drift_exclude_patterns` | `[]` | Drift filters for generated projects. |

`blocks[]`:

| Key | Default | Description |
|---|---|---|
| `block_name` | none | Name used by the `block-filters` input in drift mode. |
| `include` | required | Glob the directory must match. |
| `exclude` | none | Glob the directory must not match. |
| `include_patterns`, `exclude_patterns` | `[]` | As on a project. |
| `opentofu` | `false` | As on a project. |
| `workflow` | `default` | As on a project. |
| `workspace` | `default` | As on a project. |
| `aws_role_to_assume`, `aws_cognito_oidc` | none | As on a project. |
| `drift_include_patterns`, `drift_exclude_patterns` | `[]` | As on a project. |

### `aws_role_to_assume`

Roles assumed through the GitHub OIDC token for one project.

| Key | Default | Description |
|---|---|---|
| `aws_role_region` | `us-east-1` | Region of the STS endpoint. |
| `state` | none | Role ARN for `init` (state backend access). |
| `command` | none | Role ARN for `plan` / `apply`. |

### `aws_cognito_oidc`

Credentials obtained through a Cognito identity pool instead of STS.

| Key | Default | Description |
|---|---|---|
| `aws_account_id` | required | AWS account id. |
| `aws_region` | required | Region of the identity pool. |
| `cognito_identity_pool_id` | required | Identity pool id. |
| `session_duration` | none | Session duration in seconds. |

### Keys accepted and ignored

`git_timeout`, `telemetry`, `comment.per_project_status` (top level); `layer`,
`alias`, `block_name` (`projects[]`).

### Keys rejected

Present in `digger.yml` but refused at load time:

| Location | Keys |
|---|---|
| top level | `respect_layers`, `delete_prior_comments`, `disable_digger_apply_comment`, `disable_digger_apply_status_check`, `comment_render_mode`, `trusted_appIDs`, `report_terraform_outputs`, `reporting`, `apply_after_merge`, `mention_drifted_projects_in_pr` |
| `projects[]` | `terragrunt`, `pulumi`, `pulumi_stack`, `workflow_file`, `generated`, `drift_terragrunt_parallelism` |
| `generate_projects` | `terragrunt`, `terragrunt_parsing`, `drift_terragrunt_parallelism` |
| `generate_projects.blocks[]` | `terragrunt`, `root_dir`, `terragrunt_parsing`, `workflow_file`, `drift_terragrunt_parallelism` |

## Binary: flags and environment variables

The action sets all of these from its inputs. When running `nikama`
directly, the first variable of each list that is set and non-empty wins;
an empty value counts as unset.

| Flag | Environment variables | Description |
|---|---|---|
| `--config` | `NIKAMA_CONFIG`, `DIGGER_FILENAME` | Configuration file path. |
| `--max-parallel` | `NIKAMA_MAX_PARALLEL` | Concurrent plans. |
| `--log-level` | `NIKAMA_LOG_LEVEL`, `DIGGER_LOG_LEVEL` | Log level (default `INFO`). |
| `--repo-root` | `NIKAMA_REPO_ROOT` | Repository root (default: current directory). |
| `--local` | `NIKAMA_LOCAL` | Skip the GitHub Actions detection, for local manual runs. |
| `--validate` | | Load and validate the configuration, print the projects, exit. |

Where a variable has both a `NIKAMA_*` name and a Digger-compatible name
(`DIGGER_*`, `INPUT_DIGGER_*`, `INPUT_DRIFT_*`), the `NIKAMA_*` name wins when
both are set; the Digger name is a fallback. Empty values count as unset.

| Environment variable | Description |
|---|---|
| `NIKAMA_GITHUB_TOKEN`, `DIGGER_GITHUB_TOKEN`, `GITHUB_TOKEN` | GitHub API token (required for PR mode), in that order of precedence. A `DIGGER_GITHUB_TOKEN` left in the workflow `env:` wins over the `github-token` input (which the action passes as `GITHUB_TOKEN`). |
| `GITHUB_CONTEXT` | JSON of the `github` context (required for PR mode). |
| `GITHUB_REPOSITORY` | `owner/repo`. |
| `GITHUB_API_URL` | API base for GitHub Enterprise; ignored when it points at `api.github.com`. |
| `NIKAMA_MODE`, `INPUT_DIGGER_MODE` | `` / `manual` / `drift-detection`. |
| `NIKAMA_COMMAND`, `INPUT_DIGGER_COMMAND` | Manual mode command. |
| `NIKAMA_PROJECT`, `INPUT_DIGGER_PROJECT` | Manual mode project. |
| `INPUT_DIGGER_BLOCK_FILTERS` | Drift mode block filter. |
| `NIKAMA_CONFIG_REF` | Trusted configuration ref (`config-ref`). |
| `NIKAMA_ALLOW_RUN_STEPS` | `false` refuses `run:` steps. |
| `NIKAMA_ALLOWED_PROVIDERS` | Provider allowlist. |
| `LOCK_PROVIDER` | `aws` (also `dynamodb`), `s3`, `gcp` or `noop`. |
| `DISABLE_LOCKING` | `true` disables locks regardless of `LOCK_PROVIDER`. |
| `NIKAMA_LOCK_TABLE`, `DIGGER_LOCK_TABLE` | DynamoDB table name. |
| `AWS_S3_LOCK_BUCKET` | S3 lock bucket. |
| `GOOGLE_STORAGE_LOCK_BUCKET` | Cloud Storage lock bucket. |
| `PLAN_UPLOAD_DESTINATION` | `aws`, `gcp` or empty. |
| `AWS_S3_BUCKET` | S3 plan bucket. |
| `PLAN_UPLOAD_S3_ENCRYPTION_ENABLED`, `PLAN_UPLOAD_S3_ENCRYPTION_TYPE`, `PLAN_UPLOAD_S3_ENCRYPTION_KMS_ID` | S3 plan encryption. |
| `GOOGLE_STORAGE_PLAN_ARTEFACT_BUCKET` | Cloud Storage plan bucket. |
| `REPORTING_STRATEGY` | `comments_per_run`, `latest_run_comment` or `per_project`. |
| `NIKAMA_DRIFT_SLACK_WEBHOOK_URL`, `INPUT_DRIFT_DETECTION_SLACK_NOTIFICATION_URL` | Slack webhook for drift notifications. |
| `NIKAMA_DRIFT_GITHUB_ISSUES`, `INPUT_DRIFT_GITHUB_ISSUES` | Drift notifications as GitHub Issues; enabled when either is `true`. |
| `TF_PLUGIN_CACHE_DIR` | When set, `init` steps run one at a time. |
| `NIKAMA_SERIALIZE_INIT` | `true` also runs `init` steps one at a time. |
| `TF_CLI_CONFIG_FILE` | Existing Terraform CLI configuration; kept and extended when `allowed-providers` is set. |

`DIGGER_TOKEN`, `DIGGER_HOSTNAME`, `DIGGER_ORGANISATION` and `DIGGER_RUN_SPEC`
are backend-only and ignored.

## Variables available to `run:` steps

| Variable | Description |
|---|---|
| `PROJECT_NAME` | Name of the project being run. |
| `NIKAMA_OUT`, `DIGGER_OUT` | File whose contents are appended to the project's row in the PR comment. |
| `NIKAMA_PLANFILE`, `DIGGER_PLANFILE` | Path of the plan file, after a `plan` step. |
| `TF_DATA_DIR` | Per-project Terraform data directory set by nikama. |
| `TF_IN_AUTOMATION` | Always `true`. |

`env_vars.commands` entries and the workflow's own environment are passed
through as well.
