# Digger（バックエンドレスモード）からの移行

[English](MIGRATION.md)

このガイドは、Digger を `no-backend: true` で運用しているリポジトリが tf-nikama に
切り替えるためのものです。Digger のバックエンド／オーケストレーター、Terragrunt、
Pulumi、Azure を使っている場合、tf-nikama はドロップインリプレースメントには
なりません。先に [対応していないもの](#対応していないもの) を確認してください。

要約:

1. `digger.yml` を `nikama.yml` にリネームし、未対応のキーを削除します。
2. ワークフローの `diggerhq/digger` ステップを、Terraform のセットアップステップと
   `kangaechu/tf-nikama` に置き換えます。
3. DynamoDB のロックテーブル、S3 の plan バケット（GCP なら Cloud Storage の
   バケット）、ブランチ保護ルール、PR コメントの使い方はそのままです。すべて
   変更なしで引き継がれます。

## 互換性の概要

| 領域 | 引き継げるか | 備考 |
|---|---|---|
| `digger.yml` のキーとデフォルト | はい | 同じスキーマです。未対応のキーは無視されず、読み込み時に拒否されます。 |
| DynamoDB ロックテーブル | はい | 同じテーブル名（`DiggerDynamoDBLockTable`）、同じキースキーマです。オープンな PR が保持しているロックは有効なままです。 |
| S3 plan 保存 | はい | 同じバケット変数、同じオブジェクトキー形式 `<owner>-<repo>-<pr>-<project>.tfplan` です。nikama はさらに PR ヘッドコミットをオブジェクトメタデータ（`x-amz-meta-head-sha`）に記録し、別のコミットの plan の apply を拒否します。Digger が書いた plan にはメタデータがないため、警告付きで apply されます。 |
| GCS ロックと plan 保存 | はい | 同じ `GOOGLE_STORAGE_LOCK_BUCKET` / `GOOGLE_STORAGE_PLAN_ARTEFACT_BUCKET` 変数、同じオブジェクト名、同じ `LockId` メタデータキーです。ロックオブジェクトは事前条件付きで作成されるようになったため、2 つの実行が同時にロックを取得することはなくなりました。plan オブジェクトには `head-sha` メタデータが付き、S3 と同じコミット確認が行われます。 |
| PR コメントコマンド | はい | `digger plan|apply|lock|unlock|help` はそのまま動きます。`nikama ...` も受け付けられ、`wakeup_word` で両方を置き換えられます。 |
| コミットステータスのコンテキスト | はい | 引き続き `digger/plan` と `digger/apply` なので、ブランチ保護ルールの変更は不要です。 |
| 環境変数 | はい | `INPUT_DIGGER_*`、`LOCK_PROVIDER`、`PLAN_UPLOAD_*`、`REPORTING_STRATEGY`、`DIGGER_FILENAME`、`DIGGER_GITHUB_TOKEN` は従来どおり読み込まれます。 |
| `DIGGER_PR_NUMBER` | はい | 引き続き `GITHUB_ENV` 経由でエクスポートされます。ステップ出力 `pr_number` としても利用できます。 |
| ワークフローファイル（`.github/workflows/*.yml`） | いいえ | アクションの入力が異なり、Terraform や AWS 認証情報のインストールは行いません。後述を参照してください。 |
| PR コメントのレイアウト | いいえ | 実行ごとに 1 件のコメントを、その場で更新します。オープンな PR にある既存の Digger コメントはそのまま残ります。 |

## ステップ 1: 設定ファイル

ファイルをリネームします:

```sh
git mv digger.yml nikama.yml
```

tf-nikama は引き続き `digger.yml` をフォールバックとして読み込み、警告をログに
出します。そのため、リネームは後の PR で行っても構いません。このフォールバックは
v1.0 で削除する予定です。

次に検証します。Digger と違い、tf-nikama は理解できないキーがあると即座に
失敗します:

```sh
nikama --validate --local
```

エラーになるキーを、出現箇所ごとにまとめます:

| 場所 | キー |
|---|---|
| トップレベル | `respect_layers`、`delete_prior_comments`、`disable_digger_apply_comment`、`disable_digger_apply_status_check`、`comment_render_mode`、`trusted_appIDs`、`report_terraform_outputs`、`reporting`、`apply_after_merge`、`mention_drifted_projects_in_pr` |
| `projects[]` | `terragrunt`、`pulumi`、`pulumi_stack`、`workflow_file`、`generated`、`drift_terragrunt_parallelism` |
| `generate_projects` | `terragrunt`、`terragrunt_parsing`、`drift_terragrunt_parallelism` |
| `generate_projects.blocks[]` | `terragrunt`、`root_dir`、`terragrunt_parsing`、`workflow_file`、`drift_terragrunt_parallelism` |

これらの多くはバックエンドレスモードでは効果のなかったバックエンド専用の設定
なので、削除しても何も変わりません。`git_timeout` と `telemetry` は受け付けられ、
無視されます。

設定に関する挙動の違いを 2 点知っておいてください:

- **`workflow_configuration` はデフォルトで補完されます。** Digger ではこの
  ブロックを書くと 3 つの `on_*` キーすべてが必須でした。tf-nikama は欠けている
  キーをデフォルト（`nikama plan` / `nikama unlock` / `nikama unlock`）で補います。
  `digger ...` と書かれた項目もそのまま動きます。
- **設定ファイルがないとエラーになります。** Digger は空の設定で続行しましたが、
  tf-nikama は終了コード 1 で終了します。

Digger に相当するものがない、任意の追加設定:

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

スキーマの全体は [`examples/nikama.yml`](../examples/nikama.yml) を参照してください。

## ステップ 2: ワークフローファイル

典型的な Digger のバックエンドレスワークフローは次のようなものです:

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

同等の tf-nikama ワークフロー:

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
      - uses: kangaechu/tf-nikama@v0.9.0
        with:
          upload-plan-destination: aws
          upload-plan-destination-s3-bucket: my-plan-bucket
```

何が変わり、なぜ変わったか:

- **Terraform / OpenTofu はアクションがインストールしません。** Digger の
  `setup-terraform`、`terraform-version`、`setup-opentofu`、`opentofu-version`
  入力はなくなりました。`hashicorp/setup-terraform` または
  `opentofu/setup-opentofu` を自分で追加してください。tf-nikama が実際の終了
  コードを読めるよう、ラッパーは無効にします（`terraform_wrapper: false`）。
- **コメント時の PR ヘッドのチェックアウトは明示的に行います。** Digger は
  `configure-checkout: true` の裏でこれを行っていました。tf-nikama は上記の
  `actions/checkout` の `ref:` 式に依存します。
- **クラウドの認証情報はワークフロー側で用意します。** Digger の `setup-aws`、
  `aws-role-to-assume`、`aws-access-key-id` などはなくなりました。引き続き
  `aws-actions/configure-aws-credentials` を使ってください。`nikama.yml` の
  プロジェクトごとの `aws_role_to_assume` と `aws_cognito_oidc` はそのまま動きます。
  Google Cloud では、`setup-google-cloud` と `google-*` の認証入力を
  `google-github-actions/auth` に置き換えます。tf-nikama はそれが書き出す認証情報
  （`GOOGLE_APPLICATION_CREDENTIALS`）をロックと plan のバケットに使います。
- **`GITHUB_CONTEXT` と `GITHUB_TOKEN` はアクションが設定します。** `env:`
  ブロックは不要になりました。別のトークンを使う場合は `github-token` 入力で
  渡してください。
- **`no-backend: true` はなくなりました。** オフにすべきバックエンドモードが
  ありません。
- **必要な権限が減りました。** PR の実行に `actions: write` と `issues: write` は
  不要です。`issues: write` は `drift-github-issues: true` でドリフト検知を使う
  場合にだけ、`contents: write` は `auto_merge` を使う場合にだけ追加してください。
  ベースブランチが push できる人を制限している場合、`GITHUB_TOKEN` では
  まったくマージできません。GitHub App の設定は
  [CONFIGURATION.ja.md](CONFIGURATION.ja.md) の「自動マージとブランチ保護」を参照してください。
- **プラグインキャッシュ。** `cache-dependencies: true` は `TF_PLUGIN_CACHE_DIR`
  を設定し、`actions/cache` を組み込んでいました。tf-nikama はこれを行いません。
  プロバイダのキャッシュが必要なら、`TF_PLUGIN_CACHE_DIR` を設定して
  `actions/cache` を自分で追加してください。この変数が設定されていると、
  キャッシュの整合性を保つため `init` ステップは 1 つずつ実行されます（plan は
  引き続き並列です）。

### アクション入力の対応表

| Digger の入力 | tf-nikama の入力 | 備考 |
|---|---|---|
| `no-backend` | （なし） | 常にバックエンドレスです。 |
| `setup-terraform`、`terraform-version`、`setup-opentofu`、`opentofu-version`、`setup-tfenv` | （なし） | セットアップ用アクションを使ってください。 |
| `setup-aws`、`aws-*` | （なし） | `aws-actions/configure-aws-credentials` を使ってください。 |
| `setup-google-cloud`、`google-auth-credentials`、`google-workload-identity-provider*`、`google-service-account` | （なし） | `google-github-actions/auth` を使ってください。 |
| `google-lock-bucket`、`upload-plan-destination-gcp-bucket` | 同名 | 変更なし。ロックバケットには `lock-provider: gcp` を設定してください。 |
| `setup-azure`、`upload-plan-destination-azure-*` | （なし） | Azure は未対応です。 |
| `setup-terragrunt`、`terragrunt-version`、`setup-checkov`、`checkov-version` | （なし） | 未対応です。 |
| `configure-checkout` | （なし） | `actions/checkout` に `ref:` を設定してください。 |
| `cache-dependencies` | （なし） | `TF_PLUGIN_CACHE_DIR` を自分で設定してください。 |
| `digger-filename` | `config-file` | 環境変数 `DIGGER_FILENAME` も引き続き有効です。 |
| `digger-token`、`digger-hostname`、`digger-organisation`、`digger-spec` | （なし） | バックエンド専用。`env` に設定されていても無視されます。 |
| `disable-locking` | `lock-provider: noop` | 環境変数 `DISABLE_LOCKING=true` も使えます。 |
| （なし） | `lock-provider` | 新規。`aws`（デフォルト）、`s3`、`gcp`、`noop`。Digger は `google-lock-bucket` が設定されていると `gcp` を選んでいました。`s3` は DynamoDB の代わりに S3 バケット（`aws-lock-bucket`）にロックを置きます。Digger に相当するものがないため、オープンな PR が DynamoDB のロックを保持していない状態になってから切り替えてください。 |
| （なし） | `lock-table` | 新規。デフォルトは `DiggerDynamoDBLockTable` です。 |
| `upload-plan-destination` | `upload-plan-destination` | `aws`、`gcp`、または空。`github` と `azure` は未対応です。 |
| `upload-plan-destination-s3-*` | 同名 | 変更なし。 |
| `reporting-strategy` | `reporting-strategy` | 同じ値で、`nikama.yml` の `comment.mode` に対応します（そちらに書かれていればそれが優先）。`comments_per_run` は、実行ごとに 1 件のコメントをその場で編集する意味になりました。 |
| `mode`、`command`、`project` | `mode`、`command`、`project` | 変更なし（手動モードとドリフトモード）。 |
| `drift-detection-slack-notification-url` | 同名 | 変更なし。 |
| `drift-github-issues` | 同名 | 既定のラベルは `digger` です。`drift-github-issue-label` で変更できます。 |
| `github-token` | `github-token` | 変更なし。 |
| （なし） | `max-parallel`、`log-level`、`version` | 新規。 |
| （なし） | `config-ref`、`allow-run-steps` | 新規。PR の実行で信頼された ref から `nikama.yml` を読む／`run:` ステップを拒否する（[CONFIGURATION.ja.md](CONFIGURATION.ja.md) の「設定の信頼」）。 |
| （なし） | `allowed-providers` | 新規。列挙した Terraform プロバイダだけをインストールできる（[CONFIGURATION.ja.md](CONFIGURATION.ja.md) の「プロバイダの制限」）。 |

### 手動実行とドリフト検知のワークフロー

`workflow_dispatch`（手動）と `schedule`（ドリフト）のワークフローは、同じ
`mode`、`command`、`project` 入力を使います。上記と同様に `diggerhq/digger`
ステップを置き換え、残りはそのままにしてください:

```yaml
      - uses: kangaechu/tf-nikama@v0.9.0
        with:
          mode: manual
          command: digger apply
          project: ${{ inputs.project }}
```

```yaml
      - uses: kangaechu/tf-nikama@v0.9.0
        with:
          mode: drift-detection
          drift-detection-slack-notification-url: ${{ secrets.SLACK_WEBHOOK }}
```

ドリフトモードには通知先が 1 つ以上必要で、ない場合は終了コード 8 で終了します。

## ステップ 3: 切り替え

1. ワークフローの変更をマージします。マージ後に開かれた PR は tf-nikama を使います。
2. すでにオープンな PR には古い Digger のコメントが残ります。次の push または
   `digger plan` コメントで、その下に tf-nikama のコメントが作られます。それらの
   PR が保持しているロックは、DynamoDB テーブルが共有されているため尊重されます。
3. Digger の実行で保存された plan は、S3 のキー形式が同一なので `digger apply`
   でそのまま使われます。

移行すべき state はなく、ロックテーブルやバケットに対して実行する作業もありません。
ロールバックはワークフローの変更を revert するだけです。

## 挙動の違い

設定ではなく、目に見える動作が変わる点です。

| | Digger | tf-nikama |
|---|---|---|
| plan | 逐次 | プロジェクトごとに並列（`concurrency.max_parallel`） |
| PR コメント | プロジェクトごとに複数。サイズ上限の処理なし | 実行ごとに 1 件。プロジェクトが完了するたびに編集。65,536 文字で分割 |
| コミットステータス | 最後にだけ設定 | 開始時に `pending`、その後 `success` / `failure` |
| コメントの `-p` | プロジェクト 1 つ | 繰り返し可能。`--project`、`-d` / `--directory` も使える |
| `-p` に未知のプロジェクト | 実行が失敗 | コメントで警告し、残りは実行 |
| `digger help` | 影響するプロジェクトがあると終了コード 7 | ヘルプを投稿し、終了コード 0 |
| 通常の Issue へのコメント | API エラー | 無視し、終了コード 0 |
| 書き込み権限のないユーザーからのコメントコマンド | 実行される | コメントで拒否し、終了コード 0（[CONFIGURATION.ja.md](CONFIGURATION.ja.md) の「コマンドを実行できる人」参照） |
| `approved` 条件 | 誰のレビューでも数える | ブランチ保護と同様、書き込み権限のあるユーザーのレビューだけを数える |
| `ready_for_review` | 無視 | `on_pull_request_pushed` を実行 |
| ロックなしの plan | なし | コメントの `digger plan --no-lock`、または `no_lock_label` のラベル（[CONFIGURATION.ja.md](CONFIGURATION.ja.md) の「PR ロック」参照） |
| apply 時のみロック | なし | `pr_locks: on_apply`。`true` / `false` の意味は従来どおり（[CONFIGURATION.ja.md](CONFIGURATION.ja.md) の「PR ロック」参照） |
| コメントでの Terraform 引数 | なし | `nikama plan -- -target=...`。`allowed_plan_args` で制限（[CONFIGURATION.ja.md](CONFIGURATION.ja.md) の「コメントでの追加引数」参照） |
| ローカルモジュールの変更 | `include_patterns` にモジュールを列挙しない限りプロジェクトは影響を受けない | `autoplan_modules: true` が `module` のソースをたどる（[CONFIGURATION.ja.md](CONFIGURATION.ja.md) の「ローカルモジュール」参照） |
| プロジェクトの `branch` | 受け付けるがプロジェクト選択には使わない | PR のベースブランチに対する正規表現。一致しないプロジェクトは影響を受けない（[CONFIGURATION.ja.md](CONFIGURATION.ja.md) の「ベースブランチフィルタ」参照） |
| `dependency_configuration: hard` | CLI では下流への展開なし | 下流のプロジェクトも plan される |
| plan 要約の replace | create または delete として数える | 独自の列（`±`） |
| `destroy` の失敗（手動モード） | 成功として報告 | 失敗として報告 |
| `mergeable_state: blocked` | チェックランを確認 | レビュー判定と、自身（`digger/*`）以外の*必須*チェックラン・コミットステータスを確認（実行中の自ワークフローのジョブは除外） |
| テレメトリ | デフォルトで有効 | なし |

## 対応していないもの

tf-nikama は AWS と Google Cloud 上の Terraform / OpenTofu を対象としています。
次の Digger の機能は対象外で、起動時または設定読み込み時に拒否されます:

- Digger バックエンド／オーケストレーター（`DIGGER_TOKEN`、`DIGGER_HOSTNAME`、run spec）
- Terragrunt（`terragrunt: true` の `generate_projects` を含む）と Pulumi
- Azure のロックと plan 保存
- GitHub Artifacts への plan 保存（`upload-plan-destination: github`）
- OPA / rego ポリシーと Checkov
- GitLab、Bitbucket、Azure DevOps（GitHub Enterprise Server は未検証。`GITHUB_API_URL` は読み込まれます）

これらに依存している場合は、Digger を使い続けてください。

## 困ったときは

バックエンドレスの Digger で動いていたはずのキーを `nikama --validate` が拒否する
場合や、PR の挙動が上の表と異なる場合は、`nikama.yml` の該当部分とワークフローの
ログを添えて [Issue を作成](https://github.com/kangaechu/tf-nikama/issues/new)
してください。
