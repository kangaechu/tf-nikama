# パラメータ一覧

[English](PARAMETERS.md)

設定できる項目をすべてまとめています。GitHub Action の入力、`nikama.yml` のキー、
`nikama` バイナリの環境変数とフラグです。各設定が実際にどう働くかは
[CONFIGURATION.ja.md](CONFIGURATION.ja.md) を参照してください。

- [アクションの入力](#アクションの入力)
- [アクションの出力](#アクションの出力)
- [`nikama.yml`](#nikamayml)
  - [トップレベルキー](#トップレベルキー)
  - [`projects[]`](#projects)
  - [`workflows.<name>`](#workflowsname)
  - [`generate_projects`](#generate_projects)
  - [`aws_role_to_assume`](#aws_role_to_assume)
  - [`aws_cognito_oidc`](#aws_cognito_oidc)
  - [受け付けるが無視されるキー](#受け付けるが無視されるキー)
  - [拒否されるキー](#拒否されるキー)
- [バイナリ: フラグと環境変数](#バイナリ-フラグと環境変数)
- [`run:` ステップで使える変数](#run-ステップで使える変数)

## アクションの入力

`kangaechu/tf-nikama` の入力はすべて省略可能です。

### 全般

| 入力 | デフォルト | 説明 |
|---|---|---|
| `version` | アクションの ref | インストールするリリースタグ（例: `v0.7.0`）。リリースタグ以外の ref を指定すると cargo でソースからビルドします。 |
| `mode` | `` | プルリクエストイベントでは空、`workflow_dispatch` では `manual`、スケジュール実行のドリフト検知では `drift-detection`。 |
| `command` | `` | 手動モードのみ: `digger plan`、`digger apply`、`digger destroy`。 |
| `project` | `` | 手動モードのみ: 実行するプロジェクト。 |
| `github-token` | `${{ github.token }}` | すべての GitHub API 呼び出しに使うトークン。push できる人を制限したブランチにマージするには GitHub App または PAT のトークンを渡します。 |
| `max-parallel` | `` | 同時に実行する plan の最大数。`concurrency.max_parallel` より優先されます。 |
| `log-level` | `INFO` | `ERROR`、`WARN`、`INFO`、`DEBUG`。 |

### 設定と信頼

| 入力 | デフォルト | 説明 |
|---|---|---|
| `config-file` | `` | 設定ファイルのパス。デフォルトの探索順は `nikama.yml`、`nikama.yaml`、次に `digger.yml`（警告付き）。 |
| `config-ref` | `` | プルリクエストの実行のみ: チェックアウトの代わりに、この git ref から GitHub API 経由で設定を読みます。`default` はリポジトリのデフォルトブランチです。 |
| `allow-run-steps` | `true` | `false` にすると `run:` ステップを含む設定を拒否します。 |
| `allowed-providers` | `` | カンマ区切りのプロバイダソースパターン（`hashicorp/aws, hashicorp/*`）。設定すると `init` はそれ以外のプロバイダを拒否します。 |

### PR ロック

| 入力 | デフォルト | 説明 |
|---|---|---|
| `lock-provider` | `aws` | `aws`（DynamoDB）、`s3`（S3 バケット）、`gcp`（Cloud Storage バケット）、`noop`（ロックなし）。 |
| `lock-table` | `DiggerDynamoDBLockTable` | `lock-provider: aws` の DynamoDB テーブル。ロールに `dynamodb:CreateTable` があれば初回利用時に作成されます。 |
| `aws-lock-bucket` | `` | `lock-provider: s3` の S3 バケット。 |
| `google-lock-bucket` | `` | `lock-provider: gcp` の Cloud Storage バケット。 |

### plan の保存

| 入力 | デフォルト | 説明 |
|---|---|---|
| `upload-plan-destination` | `` | `aws`（S3）、`gcp`（Cloud Storage）、または空。空の場合は apply の直前に生成した plan を使います。 |
| `upload-plan-destination-s3-bucket` | `` | plan ファイルを置く S3 バケット。 |
| `upload-plan-destination-s3-encryption-enabled` | `false` | 保存する plan のサーバーサイド暗号化。 |
| `upload-plan-destination-s3-encryption-type` | `AES256` | `AES256` または `KMS`。 |
| `upload-plan-destination-s3-encryption-kms-key-id` | `` | タイプが `KMS` のときの KMS キー ID。 |
| `upload-plan-destination-gcp-bucket` | `` | plan ファイルを置く Cloud Storage バケット。 |

### レポートとドリフト検知

| 入力 | デフォルト | 説明 |
|---|---|---|
| `reporting-strategy` | `comments_per_run` | `comments_per_run`（実行ごとに 1 件のコメントをその場で編集）、`latest_run_comment`、`per_project`。 |
| `drift-detection-slack-notification-url` | `` | ドリフト通知用の Slack incoming webhook。 |
| `drift-github-issues` | `false` | ドリフトしたプロジェクトごとに GitHub Issue を作成・更新します（`issues: write` が必要）。 |
| `block-filters` | `` | ドリフトモード: 対象にする `generate_projects` のブロック名（カンマ区切り）。 |

## アクションの出力

| 出力 | 説明 |
|---|---|
| `pr_number` | 実行対象のプルリクエスト番号。`GITHUB_ENV` 経由で `DIGGER_PR_NUMBER` としてもエクスポートされます。 |

## `nikama.yml`

未知のキーはエラーになります。`config-ref` が設定されていない限りチェックアウトから
読み込まれます。完全な例は [`../examples/nikama.yml`](../examples/nikama.yml) に
あります。

### トップレベルキー

| キー | デフォルト | 説明 |
|---|---|---|
| `projects` | `[]` | プロジェクトの一覧。[`projects[]`](#projects) 参照。 |
| `workflows` | `{default: ...}` | 名前付きワークフロー。[`workflows.<name>`](#workflowsname) 参照。`default` ワークフローは常に存在します。 |
| `generate_projects` | なし | ディレクトリからプロジェクトを自動検出します。[`generate_projects`](#generate_projects) 参照。 |
| `dependency_configuration.mode` | `hard` | `hard`: プロジェクトの変更で、下流の `depends_on` プロジェクトも plan します。`soft`: 依存関係は apply の順序にだけ使います。 |
| `dependency_configuration.on_failure` | `skip_dependents` | プロジェクトが失敗したときの残りの apply の扱い。`skip_dependents`: そのプロジェクトに（直接または推移的に）依存するプロジェクトをスキップし、無関係なプロジェクトは apply します。`abort`: 以降の依存レベルをすべてスキップします。`continue`: 依存するプロジェクトもそのまま apply します。[CONFIGURATION.ja.md](CONFIGURATION.ja.md) の「実行の流れ」参照。 |
| `traverse_to_nested_projects` | `true` | `generate_projects` が、それ自体がプロジェクトであるディレクトリの中まで探索するかどうか。 |
| `auto_merge` | `false` | すべての apply が成功したらプルリクエストをマージします。 |
| `auto_merge_strategy` | `squash` | `squash`、`merge`、`rebase`。 |
| `pr_locks` | `on_plan` | `on_plan` / `true`: 変更のある最初の plan でロック（変更のない plan はロックを返す）。`on_apply`: apply でのみロック。`disabled` / `false`: ロックしない。 |
| `allow_draft_prs` | `false` | `false` の場合、ドラフトのプルリクエストは何も実行せず、ロックも持ちません。 |
| `no_lock_label` | なし | このラベルが付いたプルリクエストは、ロックを取らずに plan します。 |
| `wakeup_word` | `[nikama, digger]` | コメントコマンドの先頭の単語（文字列またはリスト）。デフォルトを置き換えます。 |
| `autoplan_modules` | `false` | プロジェクトが参照するローカルモジュール（`./` または `../` のソース、推移的にたどる）をプロジェクトの一部として扱います。 |
| `allowed_plan_args` | `[]` | コメントが `--` の後ろで `terraform plan` に渡せる引数のパターン。空なら `--` は拒否されます。 |
| `allowed_providers` | `[]` | この設定が許可するプロバイダソースパターン。`allowed-providers` 入力を狭めることしかできません。 |
| `drift_include_patterns` | `[]` | `dir` が一致するプロジェクトをドリフト検知の対象にします。 |
| `drift_exclude_patterns` | `[]` | `dir` が一致するプロジェクトをドリフト検知から外します。 |
| `concurrency.max_parallel` | `4` | 同時に実行する plan の数。 |
| `concurrency.apply_parallelism` | `1` | 同じ依存レベル内で同時に実行する apply の数。 |
| `timezone` | `UTC` | PR コメントの実行開始時刻に使う IANA タイムゾーン名（例: `Asia/Tokyo`）。夏時間にも対応。無効な名前はエラー。 |
| `comment.mode` | `single` | `single`: 実行ごとに 1 件のコメント。`per_project`: プロジェクトごとに 1 件。`latest`: 1 件のコメントを毎回の実行で上書き。 |
| `comment.show_empty_plans` | `table` | 変更のないプロジェクトの表示。`table` は行だけ、`hide` は行を消す、`details` は plan 出力も含める。 |
| `comment.per_project_status` | `false` | 互換性のために受け付けますが、使われません。 |

### `projects[]`

| キー | デフォルト | 説明 |
|---|---|---|
| `name` | 必須 | プロジェクト名。`-p` や `depends_on` で使います。 |
| `dir` | 必須 | リポジトリルートからの相対ディレクトリ。 |
| `workspace` | `default` | Terraform のワークスペース。 |
| `workflow` | `default` | 使用する `workflows` 内のワークフロー名。 |
| `opentofu` | `false` | `terraform` の代わりに `tofu` を実行します。 |
| `include_patterns` | `[]` | 追加の glob パターン。一致する変更ファイルがあればプロジェクトが影響を受けます。 |
| `exclude_patterns` | `[]` | glob パターン。一致する変更ファイルはプロジェクトに影響しません。 |
| `depends_on` | `[]` | このプロジェクトより先に apply すべきプロジェクト。 |
| `branch` | なし | プルリクエストのベースブランチに対して照合する正規表現。一致しない PR はこのプロジェクトに影響しません。 |
| `apply_requirements` | `[mergeable]` | `mergeable`、`approved`、`undiverged` の組み合わせ。apply の前にそれぞれ確認されます。 |
| `drift_detection` | `true` | ドリフト実行にこのプロジェクトを含めます。 |
| `drift_include_patterns`、`drift_exclude_patterns` | `[]` | プロジェクト単位のドリフトフィルタ。 |
| `aws_role_to_assume` | なし | [`aws_role_to_assume`](#aws_role_to_assume) 参照。 |
| `aws_cognito_oidc` | なし | [`aws_cognito_oidc`](#aws_cognito_oidc) 参照。 |
| `layer`、`alias`、`block_name` | `0`、なし、なし | `digger.yml` との互換性のために受け付けますが、使われません。 |

### `workflows.<name>`

ワークフローを `null` にすると組み込みのステップを使います（`plan` = `init`、`plan`、
`apply` = `init`、`apply`）。

| キー | デフォルト | 説明 |
|---|---|---|
| `env_vars.state[]` | `[]` | `init`（state バックエンド）用の環境変数。各要素は `name` と、`value` または `value_from`（ワークフローの環境変数名）。nikama 自身が設定する変数（`allowed-providers` 用の `TF_CLI_CONFIG_FILE`、`TF_DATA_DIR`）が同名の指定より優先されます。 |
| `env_vars.commands[]` | `[]` | `plan` / `apply` / `run` ステップ用の環境変数。形式は同じ。 |
| `plan.steps[]` | `[init, plan]` | plan ステージのステップ。後述。 |
| `plan.filter_regex`、`apply.filter_regex` | なし | 正規表現。ステージ出力の一致箇所はログとコメントで `<REDACTED>` に置き換えられます。 |
| `apply.steps[]` | `[init, apply]` | apply ステージのステップ。 |
| `workflow_configuration.on_pull_request_pushed` | `[digger plan]` | push 時に実行するコマンド。コメントと同様に `destroy` は拒否されます。 |
| `workflow_configuration.on_pull_request_closed` | `[digger unlock]` | クローズまたはマージ時に実行するコマンド。 |
| `workflow_configuration.on_commit_to_default` | `[digger unlock]` | デフォルトブランチへのコミット時に実行するコマンド。 |
| `workflow_configuration.on_pull_request_to_draft` | `[]` | プルリクエストがドラフトに変換されたときに実行するコマンド。 |
| `workflow_configuration.skip_merge_check` | `false` | このワークフローでは `mergeable` 条件を省略します。 |

ステップは次のいずれかの形で書きます:

```yaml
- init                              # bare name
- plan:
    extra_args: ["-var-file=env.tfvars"]
- apply
  extra_args: ["-auto-approve"]     # sibling form, same meaning
- run: "echo done >> $NIKAMA_OUT"   # shell command
  shell: bash                       # optional
```

`init`、`plan`、`apply` は `extra_args` を受け付けます。plan ステージは `init` で
始まる必要があります。`allow-run-steps` が `false` のとき `run:` ステップは拒否されます。

### `generate_projects`

このレベルの `include` / `exclude` と `blocks` はどちらか一方だけを使います。
`*.tf` または `*.tofu` ファイルを含むディレクトリがプロジェクトとして扱われます。
`modules` という名前のディレクトリと隠しディレクトリはスキップされます。

| キー | デフォルト | 説明 |
|---|---|---|
| `include` | なし | ディレクトリが一致すべき glob。指定しないと何も生成されません。 |
| `exclude` | なし | ディレクトリが一致してはいけない glob。 |
| `blocks[]` | `[]` | 名前付きのグループ。各要素は下記のキーを持ちます。 |
| `aws_role_to_assume`、`aws_cognito_oidc` | なし | 生成されるすべてのプロジェクトに適用されます。 |
| `drift_include_patterns`、`drift_exclude_patterns` | `[]` | 生成されるプロジェクトのドリフトフィルタ。 |

`blocks[]`:

| キー | デフォルト | 説明 |
|---|---|---|
| `block_name` | なし | ドリフトモードの `block-filters` 入力で使う名前。 |
| `include` | 必須 | ディレクトリが一致すべき glob。 |
| `exclude` | なし | ディレクトリが一致してはいけない glob。 |
| `include_patterns`、`exclude_patterns` | `[]` | プロジェクトと同じ。 |
| `opentofu` | `false` | プロジェクトと同じ。 |
| `workflow` | `default` | プロジェクトと同じ。 |
| `workspace` | `default` | プロジェクトと同じ。 |
| `aws_role_to_assume`、`aws_cognito_oidc` | なし | プロジェクトと同じ。 |
| `drift_include_patterns`、`drift_exclude_patterns` | `[]` | プロジェクトと同じ。 |

### `aws_role_to_assume`

GitHub の OIDC トークンでプロジェクトごとに引き受けるロール。

| キー | デフォルト | 説明 |
|---|---|---|
| `aws_role_region` | `us-east-1` | STS エンドポイントのリージョン。 |
| `state` | なし | `init`（state バックエンドへのアクセス）用のロール ARN。 |
| `command` | なし | `plan` / `apply` 用のロール ARN。 |

### `aws_cognito_oidc`

STS の代わりに Cognito ID プールから取得する認証情報。

| キー | デフォルト | 説明 |
|---|---|---|
| `aws_account_id` | 必須 | AWS アカウント ID。 |
| `aws_region` | 必須 | ID プールのリージョン。 |
| `cognito_identity_pool_id` | 必須 | ID プールの ID。 |
| `session_duration` | なし | セッション時間（秒）。 |

### 受け付けるが無視されるキー

`git_timeout`、`telemetry`、`comment.per_project_status`（トップレベル）、
`layer`、`alias`、`block_name`（`projects[]`）。

### 拒否されるキー

`digger.yml` にはあるが、読み込み時に拒否されるキー:

| 場所 | キー |
|---|---|
| トップレベル | `respect_layers`、`delete_prior_comments`、`disable_digger_apply_comment`、`disable_digger_apply_status_check`、`comment_render_mode`、`trusted_appIDs`、`report_terraform_outputs`、`reporting`、`apply_after_merge`、`mention_drifted_projects_in_pr` |
| `projects[]` | `terragrunt`、`pulumi`、`pulumi_stack`、`workflow_file`、`generated`、`drift_terragrunt_parallelism` |
| `generate_projects` | `terragrunt`、`terragrunt_parsing`、`drift_terragrunt_parallelism` |
| `generate_projects.blocks[]` | `terragrunt`、`root_dir`、`terragrunt_parsing`、`workflow_file`、`drift_terragrunt_parallelism` |

## バイナリ: フラグと環境変数

アクションはこれらをすべて入力から設定します。`nikama` を直接実行する場合、
各行の変数のうち、最初に設定されていて空でないものが使われます。空の値は未設定として
扱われます。

| フラグ | 環境変数 | 説明 |
|---|---|---|
| `--config` | `NIKAMA_CONFIG`、`DIGGER_FILENAME` | 設定ファイルのパス。 |
| `--max-parallel` | `NIKAMA_MAX_PARALLEL` | 同時に実行する plan の数。 |
| `--log-level` | `NIKAMA_LOG_LEVEL`、`DIGGER_LOG_LEVEL` | ログレベル（デフォルト `INFO`）。 |
| `--repo-root` | `NIKAMA_REPO_ROOT` | リポジトリルート（デフォルト: カレントディレクトリ）。 |
| `--local` | `NIKAMA_LOCAL` | GitHub Actions の検出をスキップします。手動モードをローカルで実行するときに使います。 |
| `--validate` | | 設定を読み込んで検証し、プロジェクトを表示して終了します。 |

`NIKAMA_*` 名と Digger 互換名（`DIGGER_*`、`INPUT_DIGGER_*`、`INPUT_DRIFT_*`）の両方がある変数は、両方設定されている場合 `NIKAMA_*` が優先され、Digger 名はフォールバックです。空文字列は未設定として扱います。

| 環境変数 | 説明 |
|---|---|
| `NIKAMA_GITHUB_TOKEN`、`DIGGER_GITHUB_TOKEN`、`GITHUB_TOKEN` | GitHub API トークン（PR モードで必須）。この順で優先されます。ワークフローの `env:` に `DIGGER_GITHUB_TOKEN` が残っていると `github-token` 入力（アクションは `GITHUB_TOKEN` として渡します）より優先されます。 |
| `GITHUB_CONTEXT` | `github` コンテキストの JSON（PR モードで必須）。 |
| `GITHUB_REPOSITORY` | `owner/repo`。 |
| `GITHUB_API_URL` | GitHub Enterprise の API ベース URL。`api.github.com` を指す場合は無視されます。 |
| `NIKAMA_MODE`、`INPUT_DIGGER_MODE` | `` / `manual` / `drift-detection`。 |
| `NIKAMA_COMMAND`、`INPUT_DIGGER_COMMAND` | 手動モードのコマンド。 |
| `NIKAMA_PROJECT`、`INPUT_DIGGER_PROJECT` | 手動モードのプロジェクト。 |
| `INPUT_DIGGER_BLOCK_FILTERS` | ドリフトモードのブロックフィルタ。 |
| `NIKAMA_CONFIG_REF` | 信頼された設定の ref（`config-ref`）。 |
| `NIKAMA_ALLOW_RUN_STEPS` | `false` で `run:` ステップを拒否。 |
| `NIKAMA_ALLOWED_PROVIDERS` | プロバイダの許可リスト。 |
| `LOCK_PROVIDER` | `aws`（`dynamodb` も可）、`s3`、`gcp`、`noop`。 |
| `DISABLE_LOCKING` | `true` で `LOCK_PROVIDER` に関わらずロックを無効化。 |
| `NIKAMA_LOCK_TABLE`、`DIGGER_LOCK_TABLE` | DynamoDB テーブル名。 |
| `AWS_S3_LOCK_BUCKET` | S3 のロックバケット。 |
| `GOOGLE_STORAGE_LOCK_BUCKET` | Cloud Storage のロックバケット。 |
| `PLAN_UPLOAD_DESTINATION` | `aws`、`gcp`、または空。 |
| `AWS_S3_BUCKET` | S3 の plan バケット。 |
| `PLAN_UPLOAD_S3_ENCRYPTION_ENABLED`、`PLAN_UPLOAD_S3_ENCRYPTION_TYPE`、`PLAN_UPLOAD_S3_ENCRYPTION_KMS_ID` | S3 の plan 暗号化。 |
| `GOOGLE_STORAGE_PLAN_ARTEFACT_BUCKET` | Cloud Storage の plan バケット。 |
| `REPORTING_STRATEGY` | `comments_per_run`、`latest_run_comment`、`per_project`。 |
| `NIKAMA_DRIFT_SLACK_WEBHOOK_URL`、`INPUT_DRIFT_DETECTION_SLACK_NOTIFICATION_URL` | ドリフト通知の Slack webhook。 |
| `NIKAMA_DRIFT_GITHUB_ISSUES`、`INPUT_DRIFT_GITHUB_ISSUES` | ドリフト通知を GitHub Issue にする。どちらかが `true` なら有効。 |
| `TF_PLUGIN_CACHE_DIR` | 設定されていると `init` ステップを 1 つずつ実行。 |
| `NIKAMA_SERIALIZE_INIT` | `true` でも `init` ステップを 1 つずつ実行。 |
| `TF_CLI_CONFIG_FILE` | 既存の Terraform CLI 設定。`allowed-providers` 設定時は維持したうえで拡張されます。 |

`DIGGER_TOKEN`、`DIGGER_HOSTNAME`、`DIGGER_ORGANISATION`、`DIGGER_RUN_SPEC` は
バックエンド専用で、無視されます。

## `run:` ステップで使える変数

| 変数 | 説明 |
|---|---|
| `PROJECT_NAME` | 実行中のプロジェクト名。 |
| `NIKAMA_OUT`、`DIGGER_OUT` | この内容が PR コメントのプロジェクト行に追記されるファイル。 |
| `NIKAMA_PLANFILE`、`DIGGER_PLANFILE` | `plan` ステップ後の plan ファイルのパス。 |
| `TF_DATA_DIR` | nikama が設定するプロジェクトごとの Terraform データディレクトリ。 |
| `TF_IN_AUTOMATION` | 常に `true`。 |

`env_vars.commands` の要素とワークフロー自身の環境変数もそのまま渡されます。
