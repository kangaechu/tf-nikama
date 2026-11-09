# 設計ドキュメント: Digger バックエンドレスモードの Rust 再実装

- 状態: v0.1 実装済み（2026-11-10）。本書の仕様のうち未実装の項目は §17 の「実装状況」に記載
- 元になった調査: diggerhq/digger develop ブランチ（v0.6.155 相当）のソース調査
- プロジェクト名: `tf-nikama`（仮）。本書では「本ツール」と呼ぶ。設定ファイル名は `nikama.yml`

---

## 1. 背景と目的

Digger のバックエンドレスモード（`no-backend: true`）は、GitHub Actions 上で単一バイナリが PR イベントを受け取り、Terraform の plan / apply を実行して PR にコメントする仕組みである。現行実装には次の課題がある。

| 課題 | 現状 |
|---|---|
| 上流への変更が取り込まれない | PR が長期間マージされない（例: diggerhq/digger#2670） |
| バイナリが巨大 | リリースバイナリ約 115 MB。AWS/GCP/Azure の 3 SDK、OPA、terragrunt 本体、go-git、GitLab/Azure DevOps クライアントを静的リンク |
| 並列実行がない | `RunJobs` はジョブを依存順にソートした後、直列 for ループで実行。drift も直列 |
| コメントが多い・順序が崩れやすい | Plan output / Plan summary / Instructions を別々に投稿。65,536 文字制限の制御なし |

本ツールはバックエンドレスモードの機能だけを Rust で再実装し、小さなバイナリ、プロジェクト単位の並列 plan、整理された PR コメントを提供する。

### 1.1 設計原則

1. **GitHub Actions 上の単一バイナリ**。常駐プロセスや外部サービスを持たない。
2. **外部依存は Terraform/OpenTofu と AWS だけ**。それ以外のクラウド・IaC ツール・VCS は初回リリースでは扱わない。
3. **明示的に失敗する**。未対応の設定は黙って無視せず、設定読み込み時にエラーにする。
4. **後から足せる構造**。IaC ランナー、ロック、plan ストレージ、通知先は trait で抽象化し、実装を 1 つしか持たなくても境界は残す。
5. **既存 digger.yml をリネームするだけで使える**（対応範囲内であれば）。設定ファイル名は `nikama.yml` に変えるが、キー名・既定値・挙動は現行と揃える。

---

## 2. スコープ

### 2.1 対象（初回リリース）

| 領域 | 内容 |
|---|---|
| VCS / CI | GitHub Actions 上の GitHub リポジトリ（github.com） |
| IaC | Terraform、OpenTofu |
| 実行モード | PR イベント、manual（workflow_dispatch）、drift-detection（schedule） |
| コマンド | `digger plan`、`digger apply`、`digger lock`、`digger unlock`、`digger help`、manual のみ `digger destroy` |
| PR ロック | DynamoDB、NoOp |
| Plan ストレージ | S3（SSE-S3 / SSE-KMS）、なし |
| AWS 認証 | GitHub OIDC → STS AssumeRoleWithWebIdentity、Cognito Identity 経由、`-backend-config` 注入方式 |
| レポート | PR コメント（単一コメント upsert）、commit status、Drift の Slack webhook / GitHub Issue |
| 設定 | digger.yml の対応キー（§5） |

### 2.2 対象外（初回リリース）

| 領域 | 内容 | 備考 |
|---|---|---|
| バックエンド連携 | オーケストレータ、`DIGGER_RUN_SPEC`、`DIGGER_TOKEN`/`DIGGER_HOSTNAME`、ジョブ状態報告 | 恒久的に対象外 |
| 他 VCS | GitLab、Bitbucket、Azure DevOps、GitHub Enterprise Server | GHE は将来候補 |
| 他 IaC | Terragrunt（自動検出・実行とも）、Pulumi | 必要になった時点で追加 |
| 他クラウド | GCP（GCS ロック・ストレージ）、Azure（Table ロック・Blob） | |
| GitHub Artifacts による plan 保存 | 現行は非推奨 v3 API と `wget` 依存 | 将来 v4 API で再検討 |
| ポリシー | OPA / rego 評価、管理リポジトリ | CE バックエンドレスでは元々 NoOp（全許可） |
| テレメトリ | analytics.digger.dev への送信 | 実装しない |
| コメントの高度な集約 | プロジェクト別ステータス表のバックエンド版、ソース別グルーピング | 本ツール独自の単一コメントで置き換え |

---

## 3. 全体アーキテクチャ

### 3.1 モジュール構成（v0.1 実装）

初回は単一クレート `nikama`（バイナリ 1 つ）とし、モジュールで境界を切る。クレート分割は必要になった時点で行う。

```
src/
  main.rs / cli.rs   引数・環境変数の解釈、モード分岐、exit code
  config/            nikama.yml のパース（raw.rs）・既定値と検証（convert.rs）・プロジェクト生成（generate.rs）・影響判定（impact.rs）・依存グラフ（graph.rs）
  github/            GITHUB_CONTEXT のパース（context.rs）、REST クライアント（client.rs）、コメントコマンド解析（comment_cmd.rs）、イベント→ジョブ変換（events.rs）
  engine/            Job / Command モデル、Terraform/OpenTofu ランナー（runner.rs）、ステージ実行（exec.rs）、出力整形（output.rs）、plan JSON 集計（plan_json.rs）
  lock/              Lock trait、DynamoDB 実装、NoOp 実装、PR ロックのセマンティクス（PrLocker）
  storage/           PlanStorage trait、S3 実装、なし実装
  aws_auth.rs        GitHub OIDC トークン取得、STS、Cognito、認証戦略
  report/            コメントモデルとレンダリング（comment.rs）、単一ライター（writer.rs）、Drift 通知（drift.rs）
  modes/             PR イベント（pr_event.rs）、manual、drift、共通のジョブ実行（run.rs）
```

### 3.2 主要 trait

```rust
trait IacRunner {            // Terraform, OpenTofu
    async fn init(&self, args, env) -> Result<CmdOutput>;
    async fn plan(&self, args, env, plan_path, filter) -> Result<PlanOutcome>;
    async fn apply(&self, args, env, plan_path: Option<&Path>) -> Result<CmdOutput>;
    async fn destroy(&self, args, env) -> Result<CmdOutput>;
    async fn show_json(&self, plan_path) -> Result<String>;
    async fn select_workspace(&self, name) -> Result<()>;
}

trait Lock {                 // DynamoDb, NoOp
    async fn lock(&self, pr: u64, resource: &str) -> Result<bool>;
    async fn unlock(&self, resource: &str) -> Result<bool>;
    async fn holder(&self, resource: &str) -> Result<Option<u64>>;
}

trait PlanStorage {          // S3, Noop
    async fn store(&self, key: &str, bytes: &[u8]) -> Result<()>;
    async fn retrieve(&self, key: &str) -> Result<Option<Vec<u8>>>;
    async fn delete(&self, key: &str) -> Result<()>;
    async fn exists(&self, key: &str) -> Result<bool>;
}

trait DriftNotifier {        // SlackWebhook, GithubIssue
    async fn notify(&self, project: &str, repo: &str, plan: &str) -> Result<()>;
    async fn notify_error(&self, project: &str, repo: &str, err: &Error) -> Result<()>;
    async fn flush(&self) -> Result<()>;
}
```

### 3.3 主要 Rust 依存（想定）

clap、serde / serde_yaml / serde_json、globset（doublestar 互換の `**` マッチ）、petgraph、tokio、reqwest（rustls）、regex、tracing、aws-config、aws-sdk-dynamodb、aws-sdk-s3、aws-sdk-sts、aws-sdk-cognitoidentity。

クラウド SDK が AWS だけなので、リリースバイナリは 10〜20 MB 台を目標とする。

---

## 4. 実行フロー

### 4.1 起動と前提

1. `GITHUB_ACTIONS` が未設定なら「No CI detected」で exit 10。
2. 必須環境変数を検証: `GITHUB_TOKEN`（`DIGGER_GITHUB_TOKEN` があれば優先）、`GITHUB_CONTEXT`、`GITHUB_REPOSITORY`、`GITHUB_ACTOR`。
3. digger.yml を読み込み（§5）。失敗は exit 1。
4. `INPUT_DIGGER_MODE` で分岐。

### 4.2 PR イベントモード（`INPUT_DIGGER_MODE` が空）

1. `GITHUB_CONTEXT` をパース（§6.1）。
2. イベント種別ごとに影響プロジェクトとコマンドを決定（§6.2、§6.3）。影響なしなら exit 0。
3. 単一コメントの骨組みを投稿し、commit status を `pending` にする（§12、§13）。
4. 全ジョブ分の PR ロックを取得（§9.3）。取れなかったプロジェクトは表に「locked by #N」と記録して除外。
5. ジョブを実行（§8）。
6. コメントを最終更新、commit status を `success` / `failure` に更新。
7. `auto_merge` が有効で、全 apply 成功かつ影響プロジェクト全件を対象にしていた場合、PR をマージ（§6.5）。
8. `$GITHUB_OUTPUT` に `pr_number=<n>` を書く（現行の `GITHUB_ENV` 書き込みは互換のため同時に行う）。

### 4.3 manual モード

- `INPUT_DIGGER_COMMAND` ∈ {`digger plan`, `digger apply`, `digger destroy`}、`INPUT_DIGGER_PROJECT` 必須。
- ロックなし、PR コメントなし、stdout レポートのみ。
- `digger destroy` の唯一の経路（PR コメントからは実行できない）。

### 4.4 drift-detection モード

- `drift_detection: true` のプロジェクトを対象に、`INPUT_DIGGER_BLOCK_FILTERS`、トップレベルとプロジェクトの `drift_include_patterns` / `drift_exclude_patterns` で絞り込む。
- plan のみ実行（並列、§8）。非空 plan を通知先へ送る（§14）。
- 通知先未設定はエラー（exit 8）。

---

## 5. nikama.yml 仕様

設定ファイルの名前は `nikama.yml` とする。スキーマは現行 digger.yml の対応範囲と同一で、既存の digger.yml は（対応範囲内であれば）リネームするだけで使える。

### 5.1 読み込み

探索順:

1. `NIKAMA_CONFIG`（または `--config`）が指定されていればそのパス。互換のため `DIGGER_FILENAME` も同じ意味で受け付ける。
2. `nikama.yml`、`nikama.yaml`。両方あればエラー。
3. どちらも無ければ `digger.yml`、`digger.yaml` を **フォールバックとして** 読み、「`nikama.yml` にリネームしてください」と警告を出す。`nikama.*` と `digger.*` が両方あれば `nikama.*` を使い、`digger.*` は無視して警告。
4. いずれも無ければエラー（現行は空設定で続行するが、本ツールでは明示エラーにする）。

`digger.yml` フォールバックは移行期間のためのもので、v1.0 で削除する想定。

- YAML パースは **未知キーをエラー**にする（`deny_unknown_fields`）。
- §5.5 の非対応キーは専用メッセージ付きでエラーにする。

### 5.2 トップレベル

| キー | 型 | 既定 | 備考 |
|---|---|---|---|
| `projects` | list | `[]` | |
| `workflows` | map | `default` を自動追加 | null 値は既定 workflow として扱う |
| `generate_projects` | object | なし | §5.4 |
| `dependency_configuration.mode` | `hard` / `soft` | `hard` | hard のとき、影響プロジェクトの下流（依存先）も plan 対象に加える（現行 CLI は未実装だが、本ツールでは実装する） |
| `traverse_to_nested_projects` | bool | `true` | 生成時に入れ子ディレクトリへ降りるか |
| `auto_merge` | bool | `false` | |
| `auto_merge_strategy` | `squash` / `merge` / `rebase` | `squash` | |
| `pr_locks` | bool | `true` | false なら NoOp ロック |
| `allow_draft_prs` | bool | `false` | false のとき draft PR のイベントは無視する |
| `git_timeout` | int | `30` | 受理するが未使用（EE ポリシー用） |
| `telemetry` | bool | `true` | 受理するが未使用 |
| `drift_include_patterns` / `drift_exclude_patterns` | list | `[]` | |
| `concurrency` | object | §8.1 | **本ツール独自** |
| `comment` | object | §12.6 | **本ツール独自** |

### 5.3 project / workflow

**project**

| キー | 既定 / 規則 |
|---|---|
| `name` | 必須、一意 |
| `dir` | 必須 |
| `workspace` | `default` |
| `workflow` | `default`、`workflows` に存在すること |
| `opentofu` | `false` |
| `include_patterns` / `exclude_patterns` | doublestar glob。`.` 始まりは `dir` 基準 |
| `depends_on` | 名前が存在すること、閉路禁止 |
| `layer`、`alias`、`block_name`、`branch` | 受理、`alias` は表示に使用 |
| `apply_requirements` | `[mergeable]`。許容値 approved / mergeable / undiverged。`mergeable` を含まないと `skip_merge_check` の既定が true になる |
| `drift_detection` | `true` |
| `drift_include_patterns` / `drift_exclude_patterns` | `[]` |
| `aws_role_to_assume` | `{aws_role_region (既定 us-east-1), state, command}`。片方のみ指定時は他方にコピー |
| `aws_cognito_oidc` | `{aws_account_id, aws_region, cognito_identity_pool_id, session_duration}` |

**workflow**

- `env_vars.state[]`、`env_vars.commands[]`: `{name, value}` または `{name, value_from}`。`value_from` は実行時の環境変数から展開。
- `plan.steps[]`、`apply.steps[]`: 既定 `[init, plan]` / `[init, apply]`。
  - ステップ記法 3 種: スカラー `init`、`run: "<cmd>"`（任意で `shell:`）、`init: {extra_args: [...]}`（`plan` / `apply` も同様）。
  - `filter_regex`: 出力中の一致箇所を `<REDACTED>` に置換。
- `workflow_configuration`:
  - `on_pull_request_pushed` 既定 `[digger plan]`
  - `on_pull_request_closed` 既定 `[digger unlock]`
  - `on_commit_to_default` 既定 `[digger unlock]`
  - `on_pull_request_to_draft` 既定 `[]`
  - `skip_merge_check` 既定 `false`
  - **ブロックを書いた場合も省略キーは既定で補完する**（現行は 3 つの `on_*` が必須扱いになる非互換があるが、本ツールでは補完する）。

### 5.4 generate_projects

- トップレベル `include` / `exclude`（単一 glob）または `blocks[]`。両方の併用はエラー。
- 対象ディレクトリ: `*.tf` を含むディレクトリ。`modules` という名前のディレクトリは除外。
- block: `block_name`、`include`、`exclude`、`include_patterns`、`exclude_patterns`、`opentofu`、`workflow`、`workspace`、`aws_role_to_assume`、`aws_cognito_oidc`、`drift_include_patterns`、`drift_exclude_patterns`。
- 生成されるプロジェクト名はディレクトリパスの `/` を `_` に置換したもの。

### 5.5 エラーにするキー（初回リリース）

次のキーが現れた場合、設定読み込み時に「unsupported in this tool」の旨を出してエラー終了する。

| 分類 | キー |
|---|---|
| プロジェクト種別 | `terragrunt`、`pulumi`、`pulumi_stack` |
| generate_projects | `terragrunt`、`terragrunt_parsing`、`root_dir`、`drift_terragrunt_parallelism`（トップレベル・ブロック・プロジェクトすべて） |
| バックエンド専用 | `respect_layers`、`delete_prior_comments`、`disable_digger_apply_comment`、`disable_digger_apply_status_check`、`comment_render_mode`、`trusted_appIDs`、`report_terraform_outputs`、`reporting`、`workflow_file`（プロジェクト・ブロック） |
| 未使用 | `apply_after_merge`、`mention_drifted_projects_in_pr`、`generated` |

将来「受理して無視」に緩める可能性がある。緩和は互換性を壊さない方向なので後からで問題ない。

### 5.6 影響プロジェクト判定

- プロジェクトごとに暗黙の include `dir/**/*` を追加し、`include_patterns` / `exclude_patterns` と合わせて変更ファイルを照合する。exclude 優先。
- `dependency_configuration.mode: hard` のとき、影響プロジェクトに依存するプロジェクトも BFS で追加する。
- `.`、`..` を含むパスは正規化してから照合する。

---

## 6. GitHub 連携

### 6.1 GITHUB_CONTEXT

`toJson(github)` を受け取り、`event_name` で分岐してイベント本体を型付きでデコードする。読む項目は次に限定する。

| event_name | 使用フィールド |
|---|---|
| `pull_request` | `action`、`pull_request.{number, draft, head.ref, head.sha, base.ref, merged}`、`repository.{default_branch, full_name}`、`sender.login` |
| `issue_comment` | `issue.{number, pull_request}`、`comment.{id, body}`、`repository.{default_branch, full_name}`、`sender.login` |
| `merge_group` | 無視して exit 0 |
| `workflow_dispatch`、`schedule` | manual / drift モードで使用 |
| その他 | 「unsupported event」で exit 6 |

`issue_comment` で `issue.pull_request` が無い（Issue へのコメント）場合は PR ではないので exit 0 にする（現行は API エラーになる）。

### 6.2 pull_request → コマンド

| action | 条件 | コマンド元 |
|---|---|---|
| `closed` | merged かつ `base.ref == default_branch` | `on_commit_to_default` |
| `closed` | 上記以外 | `on_pull_request_closed` |
| `opened` / `reopened` / `synchronize` | `allow_draft_prs: false` かつ draft なら無視 | `on_pull_request_pushed` |
| `ready_for_review` | | `on_pull_request_pushed`（**現行にない追加**） |
| `converted_to_draft` | | `on_pull_request_to_draft`。空かつ `allow_draft_prs: false` なら `[digger unlock]` |
| その他 | | ジョブなし、exit 0 |

### 6.3 issue_comment → コマンド

- 本文を trim し、先頭が `digger ` で始まるものだけ処理。他は exit 0。
- サブコマンド: `plan`、`apply`、`lock`、`unlock`、`help`。`destroy` は拒否してその旨をコメント。
- フラグ（現行バックエンド版と同等、CLI 版より拡張）:
  - `-p <name>` / `--project <name>`: 複数指定可
  - `-d <dir>` / `--directory <dir>`: 複数指定可
  - 指定なしは影響プロジェクト全件
- 影響プロジェクトに含まれない名前を指定した場合はコメントで警告し、残りを実行する（現行はエラー終了）。
- `digger help` はコマンド一覧をコメントして exit 0 で終了する（現行は影響プロジェクトがあると exit 7 になる）。
- コメントに 👀 リアクションを付ける（受理の可視化。失敗しても続行）。

### 6.4 使用する GitHub API

| 用途 | API |
|---|---|
| 変更ファイル | `GET /repos/{o}/{r}/pulls/{n}/files`（ページング、rename は `previous_filename` も含める） |
| PR 情報 | `GET /repos/{o}/{r}/pulls/{n}`（head/base、merged、mergeable、mergeable_state、draft） |
| コメント | `GET/POST /repos/{o}/{r}/issues/{n}/comments`、`PATCH /repos/{o}/{r}/issues/comments/{id}` |
| リアクション | `POST /repos/{o}/{r}/issues/comments/{id}/reactions` |
| commit status | `POST /repos/{o}/{r}/statuses/{sha}`、`GET /repos/{o}/{r}/commits/{sha}/status` |
| マージ | `PUT /repos/{o}/{r}/pulls/{n}/merge` |
| Drift Issue | `GET /repos/{o}/{r}/issues?state=open&labels=digger`、`POST /repos/{o}/{r}/issues`、`PATCH /repos/{o}/{r}/issues/{n}` |

- 認証は `GITHUB_TOKEN` の Bearer。
- 403/429 とリトライ可能な 5xx は指数バックオフ（最大 5 回）。`Retry-After` と `x-ratelimit-reset` を尊重する。
- コンテンツ作成系（コメント・status・リアクション）は単一ライター経由で直列化する（§12.4）。

### 6.5 apply の前提条件と auto-merge

- apply 実行条件: `merged == true`、または `mergeable == true` かつ `mergeable_state ∈ {clean, unstable, has_hooks}`、または `skip_merge_check`。
- `mergeable_state == blocked` のとき、現行は check-run を見て「digger/apply 以外が全部 success なら可」と判定するが、本ツールでは **commit status の combined state** を見る（本ツールは check-run を作らないため）。`digger/apply` と `digger/plan` を除いた status が全て success なら可とする。
- auto-merge: `auto_merge: true` かつ全 apply 成功かつ `atLeastOneApply` かつ対象が影響プロジェクト全件のとき、combined status が success で mergeable なら `auto_merge_strategy` でマージ。

---

## 7. 実行エンジン

### 7.1 ジョブ

```
Job {
  project: ProjectRef,     // name, dir, workspace, opentofu, alias
  command: Command,        // Plan | Apply | Destroy | Lock | Unlock
  plan_stage, apply_stage: Stage,
  state_env, command_env, run_env: Map,
  pr_number: Option<u64>, event_name, requested_by, namespace ("owner/repo"),
  aws: AwsAuthConfig,
  skip_merge_check: bool,
}
```

`run_env` には `DEFAULT_BRANCH`、`PR_BRANCH`、`PROJECT_NAME`、`PROJECT_DIR`、`DIGGER_PLANFILE`、`DIGGER_OUT` を入れる。

### 7.2 ステップ実行

- `init`: ランナーの `init`。`state_env` を適用。extra_args は `$VAR` 展開。
- `plan`: ランナーの `plan`。`command_env` を適用。plan ファイルはジョブの一時ディレクトリ（§8.4）に `-out` で出力。
- `apply`: ランナーの `apply`。plan ファイルがあればそれを渡す。
- `run`: `run_env` を適用し、一時スクリプトを `bash -eo pipefail`（`shell` 指定時はそのシェル）で実行。作業ディレクトリはプロジェクトディレクトリ。
- 未知のアクションは設定読み込み時にエラー。

### 7.3 ランナー

| | Terraform (`terraform`) | OpenTofu (`tofu`) |
|---|---|---|
| init | `init <extra> -input=false -no-color` | 同左 |
| workspace | `default` 以外なら `workspace select` 失敗時に `workspace new` | 同左 |
| plan | `plan <extra> -input=false -no-color -detailed-exitcode -lock-timeout=3m -out <file>` | 同左 |
| apply | `apply <extra> -input=false -no-color -auto-approve -lock-timeout=3m [<file>]` | 同左 |
| destroy | `destroy <extra> -input=false -no-color -auto-approve -lock-timeout=3m` | 同左 |
| show | `show -no-color -json <file>` | 同左 |

- 環境は親プロセスの環境にジョブの env を上書き追加。`TF_IN_AUTOMATION=true` を常に付与。
- stdout / stderr はジョブ単位でバッファし、コンソールには `[<project>]` プレフィックス付きで流す。`filter_regex` の置換はバッファ・コンソール両方に適用。
- ログに出す引数は `-backend-config=` の `access_key` / `secret_key` / `token` をマスクする。
- plan の exit code 2 は「差分あり」として成功扱い。

### 7.4 plan 結果の解釈

- `show -json` の出力を `resource_changes[].change.actions` と `output_changes` で判定。`["no-op"]` 以外が 1 つでもあれば非空。
- 集計: create / update / delete を数える。`["delete","create"]` と `["create","delete"]` は **replace として別カウント**（現行は先頭アクションのみ見るため不正確）。
- 人間可読の plan 出力は「Terraform will perform the following actions:」または「OpenTofu will perform the following actions:」または「No changes.」から、区切り線（`─────`）までを切り出す。見つからなければ全文。ANSI は `-no-color` で抑止し、念のため除去も行う。
- plan summary 表（リソースアドレス × アクション）を Markdown で生成する（tf-summarize 相当を自前実装）。

### 7.5 destroy / lock / unlock

- destroy: `init` → `destroy`。**失敗を正しくエラーにする**（現行は常に成功扱い）。
- lock: PR ロックを取得し結果をコメント。
- unlock: PR ロックを強制解放し、保存済み plan を削除。

---

## 8. 同時実行モデル

### 8.1 設定

```yaml
concurrency:
  max_parallel: 4          # plan / drift の同時実行数。既定 4
  apply_parallelism: 1     # 依存レベル内の apply 同時実行数。既定 1（直列）
```

CLI の `--max-parallel` と環境変数 `DIGGER_MAX_PARALLEL` で上書き可。

### 8.2 plan

- 全ジョブを tokio タスクとして起動し、`max_parallel` のセマフォで制限する。依存グラフは待ち合わせに使わない（plan は state を変更しないため）。
- **`init` ステップだけプロセス内 mutex で直列化する。** 理由は §8.3。plan ステップは並列。
- 1 つのジョブの失敗は他ジョブに影響しない。

### 8.3 init を直列化する理由

Terraform の provider プラグインキャッシュ（`TF_PLUGIN_CACHE_DIR`）は公式に「concurrency safe ではなく、複数の `terraform init` が同時に走る環境での挙動は未定義」とされている。`init` は provider の zip をキャッシュディレクトリへ展開してから `.terraform/providers` へリンクするが、展開中の排他制御がないため、同時 init で次の症状が出る。

- `the cached package for ... does not match any of the checksums recorded in the dependency lock file`
- `Text file busy`
- `Failed to read any lines from plugin's stdout`（途中で切れたバイナリ）
- `.terraform.lock.hcl` への不正なハッシュ追記

半端に書かれたキャッシュは後続 run も全部失敗させ、`actions/cache` で保存されると次の run に持ち越される。現行 Digger の action.yml は `TF_PLUGIN_CACHE_DIR` を無条件に設定するため、既存ユーザー全員が該当する。

Terragrunt（Provider Cache Server の導入前は init 並列を禁止）と Atlantis（`parallel_plan` での競合報告と init 直列化の要望）も同じ問題に当たっている。

対策比較:

| 方式 | 採否 |
|---|---|
| init を mutex で直列化、plan は並列 | **採用**。init は数秒〜十数秒で plan 本体より短く、並列化の効果はほぼ残る |
| ジョブごとに別キャッシュディレクトリ | 不採用。provider を毎回ダウンロードしキャッシュの意味がない |
| 事前にキャッシュを温める | 不採用（利用者側の運用が必要）。将来のオプション候補 |

`TF_PLUGIN_CACHE_DIR` が未設定なら mutex を外す最適化は可とする。

### 8.4 ジョブごとの一時ディレクトリ

`$RUNNER_TEMP/<tool>/<project>/` に次を置く。

- plan ファイル: `<owner>-<repo>-<pr>-<project>.tfplan`（S3 キーと同名）
- 追加出力ファイル（`DIGGER_OUT`）
- ジョブログ

同じ `dir` を複数プロジェクト（workspace 違い）で使う場合、`.terraform/` が衝突するため `TF_DATA_DIR` もジョブごとに分ける。

### 8.5 apply

- 依存グラフを Kahn 法でレベル分けする。レベル間は直列。レベル内は `apply_parallelism` で制限（既定 1）。
- 依存元（上流）の apply が失敗した場合、依存先はスキップし表に「skipped (dependency failed)」と記録する。
- apply の前提条件（§6.5）はジョブ実行前に 1 回だけ確認する。

### 8.6 drift

plan と同じく `max_parallel` で並列。通知は完了順ではなくプロジェクト名順に送る（Slack の可読性のため、完了を集めてから送る）。

---

## 9. PR ロック

### 9.1 DynamoDB スキーマ（現行互換）

| 項目 | 値 |
|---|---|
| テーブル名 | `DiggerDynamoDBLockTable`（`NIKAMA_LOCK_TABLE` で上書き可、**独自拡張**） |
| キー | `PK` (S, HASH) = `LOCK`、`SK` (S, RANGE) = `RES#<owner>/<repo>#<project>` |
| 属性 | `transaction_id` (N) = PR 番号、`timeout` (S, RFC3339) = 取得時刻 + 90 日 |
| 課金 | PAY_PER_REQUEST |
| 取得 | `UpdateItem` 条件 `attribute_not_exists(SK) OR timeout < :now` |
| 解放 | `DeleteItem` |
| 参照 | `GetItem` ConsistentRead |

- テーブルが無ければ作成し ACTIVE になるまで待つ（現行互換）。
- 現行 Digger から切り替えても、保持中のロックをそのまま引き継げる。

### 9.2 ロックのセマンティクス

- `lock()`: 保持者を確認し、他 PR が保持していればその PR の状態を見る。closed なら奪取、open なら失敗。自 PR なら成功。未保持なら条件付き書き込みで取得。
- `unlock()`: 自 PR が保持者の場合のみ解放。
- `force_unlock()`: 保持者に関わらず解放（`digger unlock` コマンド用）。
- PR close / merge 時の解放は `on_pull_request_closed` / `on_commit_to_default` の既定 `digger unlock` に任せる。
- `pr_locks: false` または `DISABLE_LOCKING=true` なら NoOp。
- manual / drift モードはロックしない。

### 9.3 2 フェーズ取得

1. 実行前に全ジョブ分の `lock()` を並列に試みる（DynamoDB の条件付き書き込みは原子的）。ホルダー PR の状態確認は PR 番号ごとにキャッシュする。
2. 取得できなかったプロジェクトはジョブから除外し、単一コメントの表に「🔒 locked by #N」と記録する。
3. 取得できたプロジェクトだけ実行する。

### 9.4 環境変数

- `LOCK_PROVIDER`: `aws`（既定）または `noop`。現行と同名。`gcp` / `azure` はエラー。
- AWS 認証は既定クレデンシャルチェーン（`aws-actions/configure-aws-credentials` が設定した環境変数）。`AWS_REGION` 必須。

---

## 10. Plan ストレージ

- `PLAN_UPLOAD_DESTINATION`: `aws` または空（なし）。`github` / `gcp` / `azure` はエラー。
- S3: `AWS_S3_BUCKET`、`PLAN_UPLOAD_S3_ENCRYPTION_ENABLED`、`PLAN_UPLOAD_S3_ENCRYPTION_TYPE` (`AES256` / `KMS`)、`PLAN_UPLOAD_S3_ENCRYPTION_KMS_ID`。現行と同名。
- キー: `<owner>-<repo>-<pr>-<project>.tfplan`（現行互換）。
- plan 後に保存、apply 前に取得、unlock 時に削除。
- ストレージなしの場合、apply は plan ファイルなしで実行する（現行互換。コメントに「plan file not stored, applying fresh」と注記する）。

---

## 11. AWS 認証（ジョブ単位）

### 11.1 OIDC トークン

`ACTIONS_ID_TOKEN_REQUEST_URL` に `audience` を付けて GET、`Authorization: bearer <ACTIONS_ID_TOKEN_REQUEST_TOKEN>`。audience は STS 用が `sts.amazonaws.com`、Cognito 用が `cognito-identity.amazonaws.com`。

### 11.2 戦略

| 戦略 | 条件 | 挙動 |
|---|---|---|
| BackendConfig（既定） | `aws_role_to_assume` あり、Cognito なし | state ロール: AssumeRoleWithWebIdentity の一時クレデンシャルを `init` の `-backend-config=access_key=...`、`secret_key=...`、`token=...` に注入。command ロール: `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` / `AWS_SESSION_TOKEN` を `command_env` に設定 |
| Cognito | `aws_cognito_oidc` あり | GitHub OIDC トークン → `GetId` → `GetCredentialsForIdentity`。`aws_role_to_assume` も指定されていれば、その資格情報で `AssumeRole`（セッション名 `<project>-state` / `<project>-command`） |
| なし | どちらもなし | 親プロセスの環境をそのまま使う |

- state / command の片方だけ設定された場合、他方へコピー。
- BackendConfig 戦略で `plan.steps[0]` / `apply.steps[0]` が `init` でない場合は設定エラーにする（現行は無検証で panic しうる）。

---

## 12. PR コメント仕様

### 12.1 方針

**1 run = 1 コメント。** run 開始時に骨組みを投稿し、ジョブ完了ごとに編集する。別々のコメント（Plan output / Plan summary / Instructions）は作らない。

### 12.2 コメントの特定

本文先頭に HTML コメントのマーカーを埋める。タイトル文字列での検索はしない。

- 通常: `<!-- digger:run=<GITHUB_RUN_ID>:attempt=<GITHUB_RUN_ATTEMPT> -->`
- `latest` モード: `<!-- digger:latest -->`（PR に 1 件だけ。run ごとに中身を差し替え）

### 12.3 レイアウト

```
<!-- digger:run=123:attempt=1 -->
### Digger plan · run [#123](<actions run url>) · 2026-11-10 12:34 UTC

| Project | Status | + | ~ | - | ± |
|---|---|---|---|---|---|
| network | ✅ no changes | 0 | 0 | 0 | 0 |
| app     | 🟡 changes   | 2 | 1 | 0 | 0 |
| db      | ❌ failed    | | | | |
| legacy  | 🔒 locked by #118 | | | | |

<details><summary><b>app</b> · plan output (+2 ~1)</summary>

```terraform
...
```

<details><summary>plan summary</summary>
| Resource | Action |
...
</details>
</details>

<details><summary><b>db</b> · error</summary>
```
...stderr...
```
</details>

---
`digger apply` · `digger apply -p <project>` · `digger unlock` · `digger help`
```

- 実行中の行は「⏳ running」。
- 行の並びはプロジェクト名順（apply は依存レベル順）。完了順にはしない。
- 空 plan は表の 1 行だけで、詳細ブロックを作らない。
- 見出しには必ずプロジェクト名を入れる。
- 操作案内は末尾に 1 回だけ。
- 列 `±` は replace の数。

### 12.4 単一ライター

- 各ジョブは結果（状態、集計、本文）を mpsc チャネルで送る。
- ライタータスクが状態を保持し、コメント全体を再レンダリングして `PATCH` する。
- デバウンス: 直近の編集から 2 秒以内の更新はまとめる。run 終了時は即時に最終編集する。
- GitHub API の編集競合と secondary rate limit を避ける。

### 12.5 サイズ予算

- GitHub のコメント上限は 65,536 文字。
- 予算 = 上限 − 表・見出し・案内の固定部分 − 安全マージン（2,000 文字）。
- 予算を非空 plan のプロジェクト数で割り、各プロジェクトの詳細ブロックの上限とする。超えた plan 出力は末尾を切り、`... (truncated, see job log)` と Actions run の URL を付ける。
- それでも 1 コメントに収まらない場合、詳細ブロックを `(part 2/N)` の追加コメントに分割する。表は常に先頭コメントに置く。マーカーは `<!-- digger:run=...:part=2 -->`。

### 12.6 設定

```yaml
comment:
  mode: single            # single（既定）| per_project（プロジェクトごとに 1 コメント）| latest（固定 1 コメントを差し替え）
  show_empty_plans: table # table（既定、表の行のみ）| hide（表からも消す）| details（詳細も出す）
```

環境変数 `REPORTING_STRATEGY` との対応: `comments_per_run` → `single`、`latest_run_comment` → `latest`、その他 → `per_project`。digger.yml の指定が優先。

### 12.7 ロック失敗・ポリシー等のメッセージ

ロック取得失敗、コマンド解釈エラー、destroy 拒否などは同じ単一コメントの表または末尾の「Notes」節に書く。別コメントは作らない。

---

## 13. commit status

- context: plan 系なら `digger/plan`、apply 系なら `digger/apply`。
- run 開始時に `pending`（description「running N projects」、target_url は Actions run）。
- 終了時に `success` または `failure`（description に「3 changed, 1 failed」等）。
- プロジェクト別 `digger/plan/<project>` はオプトイン（`comment.per_project_status: true`、既定 false）。

---

## 14. Drift 通知

### 14.1 Slack webhook

- `INPUT_DRIFT_DETECTION_SLACK_NOTIFICATION_URL`。
- 1 プロジェクト 1 メッセージ。本文 `:warning: *Infrastructure Drift Detected*` + repo + project + plan をコードブロックで。
- 4,000 文字単位で分割し、各パートでコードフェンスを閉じ直す。
- 全プロジェクト完了後、プロジェクト名順に送る。
- 実行エラーは別メッセージで通知。

### 14.2 GitHub Issue

- `INPUT_DRIFT_GITHUB_ISSUES=true`。
- タイトル `Drift detected in project: <project>`、ラベル `digger`。
- 同タイトルの open Issue があれば本文を更新、なければ作成。
- 本文は 65,536 文字で切る。

---

## 15. CLI・環境変数・exit code

### 15.1 CLI

```
nikama [--config PATH] [--max-parallel N] [--log-level LEVEL] [--repo-root DIR] [--local] [--validate]
```

- 引数なしで起動した場合は環境変数のみで動作する（GitHub Action からの呼び出し互換）。
- `--local`: GitHub Actions 検出（`GITHUB_ACTIONS`）をスキップする。manual モードをローカルで試すときに使う。
- `--validate`: 設定を読み込んでプロジェクト一覧を表示して終了する。
- `NIKAMA_SERIALIZE_INIT=true`: `TF_PLUGIN_CACHE_DIR` が未設定でも init を直列化する。

### 15.2 環境変数（現行と同名のものは互換維持）

| 変数 | 必須 | 用途 |
|---|---|---|
| `GITHUB_TOKEN` / `DIGGER_GITHUB_TOKEN` | 必須 | GitHub API |
| `GITHUB_CONTEXT` | 必須 | `toJson(github)` |
| `GITHUB_REPOSITORY`、`GITHUB_ACTOR`、`GITHUB_RUN_ID`、`GITHUB_RUN_ATTEMPT`、`GITHUB_SERVER_URL`、`RUNNER_TEMP`、`GITHUB_OUTPUT`、`GITHUB_ENV` | Actions が設定 | |
| `INPUT_DIGGER_MODE` | 任意 | 空 / `manual` / `drift-detection` |
| `INPUT_DIGGER_COMMAND`、`INPUT_DIGGER_PROJECT` | manual 時必須 | |
| `INPUT_DIGGER_BLOCK_FILTERS` | 任意 | drift のブロック絞り込み |
| `INPUT_DRIFT_DETECTION_SLACK_NOTIFICATION_URL`、`INPUT_DRIFT_GITHUB_ISSUES` | drift 時いずれか必須 | |
| `NIKAMA_CONFIG`（互換: `DIGGER_FILENAME`） | 任意 | 設定ファイルパス |
| `LOCK_PROVIDER` | 任意 | `aws`（既定）/ `noop` |
| `DISABLE_LOCKING` | 任意 | `true` で NoOp |
| `NIKAMA_LOCK_TABLE` | 任意 | DynamoDB テーブル名（独自、既定 `DiggerDynamoDBLockTable`） |
| `PLAN_UPLOAD_DESTINATION`、`AWS_S3_BUCKET`、`PLAN_UPLOAD_S3_*` | 任意 | plan ストレージ |
| `REPORTING_STRATEGY` | 任意 | §12.6 |
| `DIGGER_MAX_PARALLEL` | 任意 | §8.1 |
| `DIGGER_LOG_LEVEL` | 任意 | DEBUG / INFO / WARN / ERROR |
| `TF_PLUGIN_CACHE_DIR` | 任意 | 設定時は init を直列化 |
| `ACTIONS_ID_TOKEN_REQUEST_URL` / `_TOKEN` | OIDC 使用時 | |

`NO_BACKEND`、`DIGGER_TOKEN`、`DIGGER_HOSTNAME`、`DIGGER_ORGANISATION`、`DIGGER_RUN_SPEC` は無視する（設定されていても動作に影響しない。警告ログのみ）。

### 15.3 exit code

| code | 意味 |
|---|---|
| 0 | 正常終了、または処理対象なし（無視するイベント、影響プロジェクトなし） |
| 1 | 設定エラー（digger.yml、必須環境変数） |
| 2 | ロックプロバイダ初期化失敗 |
| 3 | GITHUB_CONTEXT パース失敗 |
| 4 | GitHub API / AWS 初期化失敗 |
| 6 | イベント処理失敗 |
| 7 | コマンド解釈失敗 |
| 8 | ジョブ実行失敗（1 件でも失敗） |
| 10 | CI 未検出 |

---

## 16. 現行 Digger との差分一覧

### 16.1 意図的な非互換

| 項目 | 現行 | 本ツール |
|---|---|---|
| 設定ファイル名 | `digger.yml` | `nikama.yml`（`digger.yml` は移行期間のフォールバック） |
| 非対応キー | 受理して無視または別経路で使用 | 設定エラー（§5.5） |
| 設定ファイル不在 | 空設定で続行 | エラー |
| `workflow_configuration` の省略キー | 必須扱い | 既定で補完 |
| コメント | 複数コメント、タイトル文字列で検索 | 単一コメント、HTML マーカー |
| コメントサイズ | 制御なし | 予算管理と分割 |
| plan 実行 | 直列 | 並列（init のみ直列） |
| `-p` フラグ | 1 件のみ | 複数、`--project`、`-d` 対応 |
| `digger help` | 影響プロジェクトがあると exit 7 | exit 0 |
| Issue へのコメント | API エラー | exit 0 |
| `ready_for_review` | 無視 | plan を実行 |
| destroy 失敗 | 成功扱い | エラー |
| replace の集計 | create または delete に算入 | 別カウント |
| commit status | pending なし | pending あり |
| `blocked` 時の mergeable 判定 | check-run を見る | commit status を見る |
| `dependency_configuration: hard` | CLI では下流展開なし | 下流展開あり |
| SetOutput | `GITHUB_ENV` に改行なし追記 | `GITHUB_OUTPUT` に書く（`GITHUB_ENV` も互換で書く） |
| テレメトリ | 既定オン | なし |

### 16.2 互換を維持する項目

digger.yml の対応キーと既定値、PR ロックの DynamoDB スキーマ、S3 の plan キー、環境変数名（`LOCK_PROVIDER`、`PLAN_UPLOAD_DESTINATION`、`INPUT_*`、`REPORTING_STRATEGY`）、コメントコマンド（`digger plan|apply|lock|unlock|help`）、commit status の context 名、workflow_configuration の既定コマンド。

---

## 17. 未決事項と実装状況

### 実装状況（v0.1）

実装済み: §4 の 3 モード、§5（厳格バリデーション、generate_projects の Terraform/OpenTofu ブロック、影響判定、hard モードの下流展開）、§6（イベント→ジョブ、コメントフラグ `-p`/`-d` 複数、GitHub REST、apply 前提条件、auto-merge）、§7（Terraform/OpenTofu ランナー、run ステップ、plan JSON 集計、replace の別カウント）、§8（plan 並列、init 直列化、apply の依存レベル実行、ジョブごとの一時ディレクトリ、共有 dir の `TF_DATA_DIR` 分離）、§9（DynamoDB 互換スキーマ、2 フェーズ取得、closed PR からの奪取）、§10（S3）、§11（OIDC → STS、Cognito、BackendConfig 戦略）、§12（単一コメント、HTML マーカー、単一ライター + 2 秒デバウンス、サイズ予算と分割、`per_project` / `latest` モード）、§13（pending → success/failure）、§14（Slack、GitHub Issue）。

未実装・簡略化: `comment.per_project_status`（プロジェクト別 commit status）は設定を受理するが未実装。`generate_projects` のトップレベル `include`/`exclude` は Digger 同様に単一 glob のみ。destroy は manual モードのみ（設計どおり）。実機検証は manual モード（plan / apply / destroy）をローカルで実施済み。PR イベントモード・DynamoDB・S3・OIDC は実際の GitHub Actions と AWS 環境での検証が未了。

### 未決事項

1. プロジェクト名は `tf-nikama`（仮）。バイナリ名・crate 名は `nikama` で実装した。
1a. **`digger` という語をどこまで残すか。** 設定ファイルは `nikama.yml` に決定済み。残りの候補は次の通りで、まとめて決めたい。
   - PR コメントコマンド: `digger plan` → `nikama plan`。移行期間は両方受け付ける案が有力。
   - commit status の context: `digger/plan` → `nikama/plan`。ブランチ保護ルールで context 名を参照しているリポジトリがあるため、切り替え時は利用者側の設定変更が必要。
   - 環境変数: `DIGGER_*`、`INPUT_DIGGER_*` → `NIKAMA_*`、`INPUT_NIKAMA_*`。Action の `with:` 入力名を本ツール側で決められるので、旧名はエイリアスとして受けるだけで足りる。
   - コメントの HTML マーカー: `<!-- digger:... -->` → `<!-- nikama:... -->`。本ツール独自なので互換不要。
   - DynamoDB テーブル名 `DiggerDynamoDBLockTable`、S3 キー形式: 既存ロック・plan を引き継ぐため **既定は現行のまま**。`NIKAMA_LOCK_TABLE` で変更可。
2. GitHub Action の提供形態（composite action で `terraform` / `tofu` のセットアップも含めるか、バイナリのダウンロードだけにするか）。現行 action.yml は AWS/GCP/Azure 認証や Checkov セットアップも抱えており、本ツールでは最小限にしたい。
3. `apply` を PR コメントからだけでなく、PR マージ時（`on_commit_to_default`）に自動実行する運用をサポートするか。現行は設定次第で可能。
4. `latest` モードで複数 run が同時に走った場合の扱い（後勝ちで良いか）。
5. GitHub Enterprise Server 対応の時期。

---

## 18. 受入テストに流用できる既存資産

| 資産 | 用途 |
|---|---|
| `libs/digger_config/*_test.go`（約 1,800 行） | digger.yml の解釈仕様の期待値 |
| `cli/cmd/digger/main_test.go`（約 1,030 行） | イベント → ジョブ変換のシナリオ |
| `cli/pkg/integration/integration_test.go` | DynamoDB ロックの統合テスト |
| `docs/ce/reference/digger.yml.mdx` | 公式の設定リファレンス |
| `docs/ce/reference/comment-args.mdx` | コメントフラグ仕様（バックエンド版） |

---

## 参考文献

- Terraform CLI Configuration File: Provider Plugin Cache — https://developer.hashicorp.com/terraform/cli/config/config-file
- hashicorp/terraform#25849, #31964, #28367, #32205（plugin cache の並行安全性）
- gruntwork-io/terragrunt#2542, #2698, #3476, #2920（init 並列の問題と Provider Cache Server）
- runatlantis/atlantis#5984, #6800（parallel_plan での競合と init 直列化）
