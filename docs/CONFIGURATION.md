# Configuration reference

[日本語版](CONFIGURATION.ja.md)

Everything that shapes a run: the keys of `nikama.yml`, the action inputs
that decide who can start a run and what it may execute, and the environment
variables and exit codes of the binary.
For a flat list of every input, key and variable with its default, see
[PARAMETERS.md](PARAMETERS.md).

- [Outdated plan comments](#outdated-plan-comments)
- [Configuration file](#configuration-file)
- [Time zone](#time-zone)
- [How a run works](#how-a-run-works)
- [Draft pull requests](#draft-pull-requests)
- [PR locks](#pr-locks)
- [Apply requirements](#apply-requirements)
- [Base branch filter](#base-branch-filter)
- [Local modules](#local-modules)
- [Wakeup word](#wakeup-word)
- [Extra arguments on comments](#extra-arguments-on-comments)
- [Lock and plan storage providers](#lock-and-plan-storage-providers)
- [Auto-merge and branch protection](#auto-merge-and-branch-protection)
- [Who can run commands](#who-can-run-commands)
- [Trusting the configuration](#trusting-the-configuration)
- [Restricting providers](#restricting-providers)
- [What protects against whom](#what-protects-against-whom)
- [Environment variables](#environment-variables)
- [Validating locally](#validating-locally)
- [Exit codes](#exit-codes)

## Outdated plan comments

In `comment.mode: single` (the default) and `per_project`, a completed plan
run collapses older plan comments as **outdated** after publishing its final
report. Every project recorded in an older comment must have a successful
replacement plan (`changes` or `no changes`) in the new run. A partial re-plan
keeps comments that still contain results for other projects. Split comments
are treated as one report scope; per-project comments can be hidden independently.

Only older, completed nikama plan comments by the same GitHub account are
eligible. Apply comments and running reports stay visible. `latest` continues
to update its existing comment in place. Comments produced before scope
metadata was introduced stay visible because their full project scope cannot
be verified (empty plans may have been omitted from the table).

Collapsing uses GitHub GraphQL `minimizeComment` with `OUTDATED`; comments
remain available to expand. If the token cannot minimize a comment, nikama
logs a warning and leaves the Terraform result unchanged.

## Configuration file

`nikama.yml` (or `nikama.yaml`) at the repository root. A `digger.yml` is
still read as a fallback with a warning. Unknown keys and keys for unsupported
features are errors. See [`../examples/nikama.yml`](../examples/nikama.yml)
for a complete example.

Top-level keys: `projects`, `workflows`, `generate_projects`,
`dependency_configuration`, `traverse_to_nested_projects`, `auto_merge`,
`auto_merge_strategy`, `pr_locks`, `allow_draft_prs`, `no_lock_label`,
`wakeup_word`, `autoplan_modules`, `allowed_plan_args`, `timezone`,
`drift_include_patterns`, `drift_exclude_patterns`, `concurrency`, `comment`,
`allowed_providers` (plus `git_timeout` and `telemetry`, accepted for
compatibility).

## Time zone

Set `timezone` to an IANA time zone name to display the start time of PR
reports (plan, apply and other commands) in your local time:

```yaml
timezone: Asia/Tokyo
```

For example, `2026-01-15 18:30 UTC` is displayed as `2026-01-16 03:30 JST`.
The default is `UTC`. Invalid names fail configuration validation. Daylight
saving time is handled automatically for regions that use it. This applies
to all comment modes (`single`, `per_project`, `latest`). Lock timestamps and
Terraform / OpenTofu output are unchanged.

## How a run works

1. The event is mapped to commands using each project's
   `workflow_configuration` (defaults: push → `nikama plan`, close / merge →
   `nikama unlock`). Draft pull requests run nothing by default (see "Draft
   pull requests").
2. Impacted projects are computed from the PR's changed files (`dir/**`,
   `include_patterns`, `exclude_patterns`, plus downstream `depends_on`
   projects in `hard` mode). With `autoplan_modules: true`, local modules the
   project uses count as part of it (see "Local modules"). A project with a
   `branch` only counts for pull requests into a matching base branch (see
   "Base branch filter").
3. One run comment is posted immediately with every project as *pending*, and
   the commit status is set to *pending*.
4. PR locks are checked for all projects up front and taken in configuration
   order (see "PR locks").
5. Before an apply, each project's `apply_requirements` are checked against
   the PR (see "Apply requirements").
6. Plans run in parallel (`concurrency.max_parallel`); applies run level by
   level in dependency order. The comment is edited as projects finish.
   An apply always uses a plan file: the stored one when plan storage is
   configured (a missing stored plan fails the job instead of applying
   blind), otherwise a plan produced immediately before the apply, whose
   summary is shown in the comment. Stored plans record the PR head commit
   they were produced from; a plan from a different commit is refused.
   A plan without changes is not applied: the project is reported as
   *no changes* and Terraform is not run for it (a stored plan remembers
   whether it was empty, so no `init` is needed to find out). Such a project
   still counts as applied for `auto_merge`.
   When an apply fails, `dependency_configuration.on_failure` decides what
   happens to the rest (default `skip_dependents`): projects that depend on
   the failed one, directly or transitively, are reported as *skipped* with
   the failed project named, while unrelated projects in later levels still
   apply. `abort` skips every later level instead (applies already running
   in the failing level finish), and `continue` applies dependents anyway.
   Applies that already ran are never rolled back.
7. The commit status is set to success / failure; `auto_merge` merges the PR
   when every apply succeeded (see "Auto-merge and branch protection" in
   README when the base branch is protected). A push that impacts no project
   sets both `digger/plan` and `digger/apply` to success, and a plan run in
   which no project has changes also sets `digger/apply` to success, so
   either can be used as a required check without blocking PRs that have
   nothing to apply. Both are statements about the whole commit, so a run
   that covers only part of the impacted projects (`-p` / `-d`, or a plan
   restricted with arguments after `--`) never sets them to success: an
   apply of a subset leaves `digger/apply` pending. When a pull request does
   impact projects but an event runs nothing for them, both contexts are left
   as they are rather than being set to success.

## Draft pull requests

With `allow_draft_prs: false` (the default) a draft pull request runs nothing
and takes no locks: pushes to it are ignored, converting an open pull request
to a draft releases its locks (`converted_to_draft`), and comment commands
other than `nikama unlock` are refused until it is marked ready for review.

```yaml
allow_draft_prs: false
```

## PR locks

PR locks are checked for all projects up front and taken in configuration
order. If any project is held by another open PR, no new lock is taken: the
held projects are reported as *locked by #N*, the others as skipped, and only
projects this PR already holds still run. On close / merge and on a bare
`nikama unlock`, every lock the PR still holds is released, not only those of
the currently impacted projects. A lock held by a different open pull request
is never released this way; a lock left behind by a closed pull request is
taken over by the next PR that needs the project. `nikama lock` takes the
locks of the impacted (or `-p` / `-d` selected) projects without planning;
`nikama unlock -p <project>` releases only that project's lock.

`pr_locks` decides when a lock is taken:

- `on_plan` (the default, also `true`) locks on the first plan, so one pull
  request holds a project from its first plan until it is closed or merged.
  A plan that finds no changes has nothing to apply and gives its lock back
  at the end of the run.
- `on_apply` lets every pull request plan freely and locks only on `apply`,
  which then holds the project until close / merge or `unlock` (later plans
  of the same PR keep it). With `on_apply`, add `undiverged` to
  `apply_requirements` so a pull request cannot apply a plan that lacks
  changes another pull request already applied and merged.
- `disabled` (also `false`) never locks.

```yaml
pr_locks: on_apply
```

A plan can skip locking altogether: `nikama plan --no-lock` on a comment, or
a pull request carrying the label named in `no_lock_label` (set it to the
label your Renovate / Dependabot configuration adds). Such a plan runs even
for projects held by another PR and releases any lock this PR holds for the
planned projects, so a pull request that only needs a look at its plan, for
example a Terraform or provider version bump across every directory, blocks
nobody. `nikama apply` takes the locks as usual; a stored plan is still bound
to the commit it was produced from.

```yaml
no_lock_label: dependencies
```

## Apply requirements

Before an apply, each project's `apply_requirements` are checked against the
PR. Each value is enforced independently:

- `mergeable`: GitHub reports the PR as mergeable. When branch protection
  reports it as *blocked*, the required reviews must be in place and every
  required check run or status other than nikama's own must have passed, so
  that a required `digger/apply` check does not block itself.
- `approved`: at least one approving review from a user with write access
  other than the author and no outstanding "changes requested"; reviews from
  other users are ignored, as branch protection ignores them.
- `undiverged`: the branch is not behind its base.

The default is `[mergeable]`; a closed, unmerged PR is always rejected.

```yaml
projects:
  - name: app
    dir: infra/app
    apply_requirements: [approved, mergeable, undiverged]
```

## Base branch filter

A project's `branch` is a regular expression matched against the pull
request's base branch (the branch the PR merges into, not the PR's own
branch). A project whose `branch` does not match is not impacted by that pull
request: no plan on push, and `-p` / `-d` on a comment do not select it (the
comment explains why). Downstream `depends_on` projects are filtered the same
way. The same directory can be registered twice with different workspaces:

```yaml
projects:
  - name: app-staging
    dir: infra/app
    workspace: staging
    branch: ^develop$
  - name: app-prod
    dir: infra/app
    workspace: prod
    branch: ^(main|release/.*)$
```

The match is unanchored, so write `^main$` rather than `main` when `main`
must not also match `maintenance`. Manual and drift runs have no pull
request and ignore the filter.

## Local modules

A project that uses a module from elsewhere in the repository is not impacted
when only that module changes, unless the directory is listed in
`include_patterns` by hand. `autoplan_modules: true` does that automatically:
the `*.tf` / `*.tofu` files of each project directory are scanned for
`module` blocks whose `source` is a local path (`./...` or `../...`), the
referenced directories are followed transitively, and a change anywhere under
them impacts the project.

```yaml
autoplan_modules: true
```

Only string-literal sources are recognised; registry, git and HTTP sources,
`*.tf.json` files and directories that do not exist in the checkout are
ignored. The scan reads the checkout, so with `config-ref` it follows the
pull request's own module references, which can only widen the set of
impacted projects. `generate_projects` never turns a `modules` directory
into a project, so the two work together.

## Wakeup word

A PR comment is treated as a command when its first non-empty line starts
with a wakeup word followed by the command: `nikama plan`, `digger apply -p
app`. By default both `nikama` and `digger` are accepted (case-insensitive).
`wakeup_word` replaces that list, so set it when another bot in the repository
reacts to the same words or when you prefer a shorter one:

```yaml
wakeup_word: tf            # only `tf plan`, `tf apply`, ...
# or several; the first one is used in help texts and comment footers
wakeup_word: [tf, nikama]
```

Each word must be a single token without whitespace. `workflow_configuration`
entries and the manual-mode `command` input accept the configured words as
well as the defaults, so `digger plan` keeps working there. Like every other
key, `wakeup_word` is read from the trusted configuration when `config-ref` is
set, so a pull request cannot change which comments start a run.

## Extra arguments on comments

Everything after `--` on a comment command is passed to `terraform plan`,
after the `extra_args` of the workflow's plan step and without environment
expansion:

```
nikama plan -p app -- -target=module.vpc
nikama plan -p app -- -replace=aws_instance.web
nikama plan -- -refresh=false
```

The feature is off until `nikama.yml` lists what may be passed. Every
argument is checked on its own against the patterns, where `*` matches any
run of characters, so write values in the `-flag=value` form:

```yaml
allowed_plan_args:
  - "-target=*"
  - "-replace=*"
  - "-refresh=false"
```

An argument outside the list, or any argument while the list is empty, is
refused with a comment and nothing runs. Like every other key,
`allowed_plan_args` is read from the trusted configuration when `config-ref`
is set. Leave out anything that changes where state lives or how it is
locked (`-state=`, `-lock=false`, `-backend-config=`).

The arguments go to every selected project, so pair `-target` and
`-replace` with `-p`; a project where the address does not exist fails on its
own row. `--no-lock` can be combined with `--`.

How `apply` picks them up depends on plan storage:

- With plan storage, the stored plan already carries the targeting, so
  `nikama apply` applies it as it is; the run comment notes the arguments the
  plan was produced with. `nikama apply -- ...` is refused, as Terraform
  does not accept planning options together with a saved plan.
- Without plan storage, `apply` plans immediately before applying, so give
  it the same arguments: `nikama apply -p app -- -target=module.vpc`.

## Lock and plan storage providers

PR locks and plan files live in your own cloud account. The lock provider is
chosen with `lock-provider`, plan storage with `upload-plan-destination`; the
two are independent, and plan storage may be left off (an apply then plans
immediately before applying).

### AWS: DynamoDB locks (default)

`lock-provider: aws` keeps locks in a DynamoDB table named by `lock-table`
(default `DiggerDynamoDBLockTable`, shared with Digger). nikama creates the
table on first use, so the role needs `dynamodb:DescribeTable` and
`dynamodb:CreateTable` besides `dynamodb:GetItem`, `dynamodb:Query`,
`dynamodb:UpdateItem` and `dynamodb:DeleteItem` on the table. If you would
rather not grant `CreateTable`, create the table yourself: partition key `PK`
(string), sort key `SK` (string), on-demand capacity.

```yaml
      - uses: kangaechu/tf-nikama@v0.8.0
        with:
          upload-plan-destination: aws
          upload-plan-destination-s3-bucket: my-plan-bucket
```

### AWS: S3 locks

`lock-provider: s3` keeps everything in S3 and needs no DynamoDB permissions.
Lock objects are created with `If-None-Match: *`, so two runs cannot both
take the same project. The role needs `s3:PutObject`, `s3:GetObject`,
`s3:DeleteObject` and `s3:ListBucket` on the lock bucket, which can be the
plan bucket.

```yaml
      - uses: kangaechu/tf-nikama@v0.8.0
        with:
          lock-provider: s3
          aws-lock-bucket: my-lock-bucket
          upload-plan-destination: aws
          upload-plan-destination-s3-bucket: my-plan-bucket
```

### Google Cloud: Cloud Storage

Replace the AWS credentials step with `google-github-actions/auth` and point
the action at Cloud Storage buckets. The service account needs
`roles/storage.objectUser` on both buckets. Lock objects and plan files are
compatible with Digger's `gcp` providers.

```yaml
      - uses: google-github-actions/auth@v3
        with:
          workload_identity_provider: projects/123456789012/locations/global/workloadIdentityPools/github/providers/github
          service_account: nikama@my-project.iam.gserviceaccount.com
      - uses: kangaechu/tf-nikama@v0.8.0
        with:
          lock-provider: gcp
          google-lock-bucket: my-lock-bucket
          upload-plan-destination: gcp
          upload-plan-destination-gcp-bucket: my-plan-bucket
```

### No locks

`lock-provider: noop` (or `pr_locks: disabled` in `nikama.yml`) runs without
locks. Use it only where two pull requests can never touch the same state.

## Auto-merge and branch protection

`auto_merge` merges the PR with the token the action runs with. By default
that is the workflow's `GITHUB_TOKEN`, so the merge is made by
`github-actions[bot]`, and two branch protection settings on the base branch
reject it:

- **Restrict who can push to matching branches** (or a ruleset that limits
  who can push or bypass). Merging counts as a push, and the GitHub Actions
  identity behind `GITHUB_TOKEN` cannot be added to that list. GitHub answers
  `405` with "You're not authorized to push to this branch" and the log shows
  `auto-merge: merge failed`. Neither more `permissions:` nor "Allow
  specified actors to bypass required pull requests" change this.
- **Require status checks to pass** with the nikama workflow job itself as a
  required check. The job cannot finish before it merges, so GitHub answers
  "Required status check ... is expected". Require the `digger/apply` commit
  status instead: nikama sets it to success before merging.

The workaround for the first case is to merge as a GitHub App, which can be
put on the allow list:

1. Create a GitHub App (organisation or user settings → Developer settings)
   with repository permissions **Contents: read and write**, **Pull
   requests: read and write**, **Commit statuses: read and write** and, for
   drift detection with `drift-github-issues: true`, **Issues: read and
   write**. No webhook is needed.
2. Install the app on the repository, then add it to "Restrict who can push
   to matching branches" (branch protection) or to the bypass list (ruleset)
   of the base branch.
3. Store the App ID and private key as repository secrets and mint an
   installation token at the start of the job. Pass it as `github-token`;
   nikama uses it for every GitHub API call, not only the merge.

```yaml
    steps:
      - uses: actions/create-github-app-token@v3
        id: app-token
        with:
          app-id: ${{ secrets.NIKAMA_APP_ID }}
          private-key: ${{ secrets.NIKAMA_APP_PRIVATE_KEY }}
      - uses: actions/checkout@v7
        # ...
      - uses: kangaechu/tf-nikama@v0.8.0
        with:
          github-token: ${{ steps.app-token.outputs.token }}
          # ...
```

The workflow `permissions:` block then only matters for `id-token: write`
(cloud credentials); the app token carries its own permissions. Unlike
`GITHUB_TOKEN`, a merge made with an app token triggers `push` workflows on
the base branch, so a workflow that applies after merge runs as usual.

A personal access token of a user on the allow list works the same way, but
ties the merge to that person's account.

## Who can run commands

A run executes the pull request's `nikama.yml` and Terraform code (providers
included) with the workflow's credentials, so starting one is equivalent to
running code in the repository. Comment commands (`nikama plan`, `nikama
apply`, ...) are therefore only accepted from users whose repository role is
`write`, `maintain` or `admin`, checked through the collaborator permission
API at run time. A command from anyone else is answered with a short comment
and the workflow exits 0. `nikama help` is answered for everyone.

The `if:` guard in the README quick start is only an optimisation that avoids
starting a runner for comments from outside the repository: `COLLABORATOR`
and `MEMBER` include read-only users, so the permission check inside nikama
is the one that counts.

`pull_request` events need no such check: GitHub gives workflows triggered by
pull requests from forks a read-only token and no OIDC token, and branches in
the repository itself can only be pushed by users with write access.

## Trusting the configuration

By default `nikama.yml` is read from the checkout, which on pull request runs
is the pull request's own version. Two inputs tighten that:

- `config-ref: default` reads the configuration from the repository's default
  branch (or any other git ref) through the GitHub API, so a pull request runs
  with the reviewed configuration: `run:` steps, workflows, `apply_requirements`
  and `aws_role_to_assume` cannot be changed by the PR itself. Changes to
  `nikama.yml` take effect once merged; a PR that adds a new project is
  planned after it lands. `generate_projects` still discovers directories in
  the checkout. The input only applies to pull request runs; manual and drift
  runs read the checkout.
- `allow-run-steps: false` refuses any configuration that contains a `run:`
  step, from whichever source it was read.

```yaml
      - uses: kangaechu/tf-nikama@v0.8.0
        with:
          config-ref: default
          allow-run-steps: "false"
```

## Restricting providers

`terraform plan` runs provider binaries, so a pull request that adds a
provider (`hashicorp/external`, anything from another registry or namespace)
runs code of its choosing with the job's credentials. `allowed-providers`
limits what can be installed:

```yaml
      - uses: kangaechu/tf-nikama@v0.8.0
        with:
          allowed-providers: "hashicorp/aws, hashicorp/random, app.terraform.io/myorg/*"
```

Entries are provider source addresses, `namespace/type` meaning
`registry.terraform.io/namespace/type`, and `*` may replace any whole
segment. nikama turns the list into a Terraform CLI configuration
(`provider_installation` with a `direct` method and that `include` list) and
passes it to every `terraform` / `tofu` invocation through
`TF_CLI_CONFIG_FILE`. `init` then refuses any provider outside the list,
including providers required by child modules, before anything is
downloaded or executed. Built-in providers (`terraform_data`) are unaffected.

- `nikama.yml` may also carry a top-level `allowed_providers:` list. It can
  only narrow the workflow input: entries not covered by `allowed-providers`
  are ignored with a warning, and a list with nothing left is an error.
  Without the workflow input the `nikama.yml` list applies on its own, which
  is only meaningful together with `config-ref`.
- An existing CLI configuration (`TF_CLI_CONFIG_FILE`, `~/.terraformrc`,
  `~/.tofurc`) is kept and extended. If it already has a
  `provider_installation` block (a filesystem or network mirror), put the
  `include` list there instead and leave `allowed-providers` unset.
  `TF_PLUGIN_CACHE_DIR` keeps working.

## What protects against whom

| Trigger | PR author | What runs their code | Protection |
|---|---|---|---|
| `issue_comment` | anyone (fork, read-only) | nothing: commands need write access | write-access check (built in) |
| `issue_comment` by a maintainer on an untrusted PR | fork, read-only | the PR's Terraform code; its `nikama.yml` unless `config-ref` | `config-ref`, `allow-run-steps`, `allowed-providers` |
| `pull_request` | fork | nothing useful: read-only token, no OIDC token, no secrets | GitHub |
| `pull_request` | write access, same repository | the PR's workflow file, so everything | not solvable in nikama: trust write access, or drop `on.pull_request` and use comment commands with branch protection, or gate the cloud role on a protected GitHub Environment |

With comment-only triggering, `config-ref: default` and `allowed-providers`
hold against users with write access too: changing either requires a merge
to the default branch.

## Environment variables

The action sets these for you; when running the binary directly:

| Variable | Purpose |
|---|---|
| `GITHUB_TOKEN`, `GITHUB_CONTEXT`, `GITHUB_REPOSITORY` | required for PR mode |
| `INPUT_DIGGER_MODE` | `` / `manual` / `drift-detection` |
| `INPUT_DIGGER_COMMAND`, `INPUT_DIGGER_PROJECT` | manual mode |
| `LOCK_PROVIDER` (`aws`/`s3`/`gcp`/`noop`), `NIKAMA_LOCK_TABLE`, `AWS_S3_LOCK_BUCKET`, `GOOGLE_STORAGE_LOCK_BUCKET`, `DISABLE_LOCKING` | PR locks |
| `PLAN_UPLOAD_DESTINATION` (`aws`/`gcp`), `AWS_S3_BUCKET`, `PLAN_UPLOAD_S3_*`, `GOOGLE_STORAGE_PLAN_ARTEFACT_BUCKET` | plan storage |
| `REPORTING_STRATEGY` | `comments_per_run` / `latest_run_comment` / `per_project`; Digger's name for `comment.mode`, which wins when written in `nikama.yml` |
| `INPUT_DRIFT_DETECTION_SLACK_NOTIFICATION_URL`, `INPUT_DRIFT_GITHUB_ISSUES` | drift notifications |
| `NIKAMA_CONFIG`, `NIKAMA_MAX_PARALLEL`, `NIKAMA_LOG_LEVEL` | CLI options |
| `NIKAMA_CONFIG_REF`, `NIKAMA_ALLOW_RUN_STEPS` | trusted configuration (see "Trusting the configuration") |
| `NIKAMA_ALLOWED_PROVIDERS` | provider allowlist (see "Restricting providers") |
| `TF_PLUGIN_CACHE_DIR` | when set, `init` steps run one at a time |

Where a `NIKAMA_*` name and a Digger-compatible name both exist, `NIKAMA_*`
wins; see [PARAMETERS.md](PARAMETERS.md#binary-flags-and-environment-variables)
for the full list.

## Validating locally

```sh
nikama --validate --local
```

## Exit codes

`0` ok / nothing to do, `1` configuration error, `2` lock provider error,
`3` context parse error, `4` initialisation error, `6` event error,
`7` command error, `8` at least one job failed, `10` not in GitHub Actions.
Events the action does not handle (for example `push` or `workflow_run`
reaching the PR mode) are ignored with exit `0`.
