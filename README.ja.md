# tf-nikama

<p align="center">
  <img src="docs/images/tf-nikama.png" alt="tf-nikama logo: a steaming stainless pot of boiling tofu" width="400">
</p>

<p align="center">
  <a href="https://github.com/kangaechu/tf-nikama/actions/workflows/ci.yml"><img src="https://github.com/kangaechu/tf-nikama/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/kangaechu/tf-nikama/releases/latest"><img src="https://img.shields.io/github/v/release/kangaechu/tf-nikama" alt="Latest release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/kangaechu/tf-nikama" alt="License"></a>
</p>

[English](README.md)

**GitHub のプルリクエスト上で Terraform / OpenTofu の plan と apply を回す。
ホストするものは何もありません。**

Atlantis のようなサーバーも、Terraform Cloud の契約も、オーケストレーターの
バックエンドも不要です。nikama は GitHub Actions のジョブ内で動く単一バイナリ
（ダウンロードサイズ約 5 MB）で、push すると影響するプロジェクトを plan し、
コメントで apply し、ロックと plan ファイルは自分のクラウドアカウントのバケットや
テーブルに置きます。

[Digger](https://github.com/diggerhq/digger) の *バックエンドレスモード* を Rust で
再実装したもので、そのまま置き換えられます。設定キー、コメントコマンド、ロックの
スキーマ、plan の保存形式に互換性があります。

## 動くとこうなります

実行ごとに 1 件のコメントを投稿し、最後のプロジェクトが終わるまで編集し続けます:

> ### nikama plan · run [#1234](#) · 2026-11-11T09:30:12Z
>
> | Project | Status | + | ~ | - | ± |
> |---|---|---|---|---|---|
> | network | ✅ no changes | 0 | 0 | 0 | 0 |
> | app | 🟡 changes | 2 | 1 | 0 | 0 |
> | batch | 🔒 locked by #1230 | | | | |
>
> <details><summary><b>app</b> · plan output</summary>
>
> ```
> Terraform will perform the following actions:
>   + aws_sqs_queue.events
>   + aws_lambda_event_source_mapping.events
>   ~ aws_lambda_function.worker
> ```
> </details>
>
> ---
> `nikama apply` · `nikama apply -p <project>` · `nikama unlock` · `nikama help`

## 日々の使い方

1. **プルリクエストを開く、または push する。** 変更ファイルに関係する
   プロジェクトをすべて plan し、上のコメントを投稿して、コミットステータス
   `digger/plan` を設定します。
2. **`nikama apply` とコメントする。** PR がマージ可能（必要なら承認済み）に
   なっていれば、保存された plan を依存関係の順に apply し、`digger/apply` の
   ステータスが success になります。
3. **マージする。** PR が持っていたロックが解放されます。`auto_merge: true` に
   すれば apply 成功後に nikama がマージします。

| コメント | 動作 |
|---|---|
| `nikama plan` | 影響するすべてのプロジェクトを plan |
| `nikama plan -p app` | プロジェクトを指定して plan（`-p` は繰り返し可、`-d <dir>` でディレクトリ指定） |
| `nikama plan --no-lock` | ロックを取らずに plan。結果を見たいだけのときに |
| `nikama plan -p app -- -target=module.vpc` | 許可リスト内の引数を `terraform plan` に渡す |
| `nikama apply` / `nikama apply -p app` | すべて、または 1 つのプロジェクトを apply |
| `nikama unlock` | この PR が持つロックをすべて解放 |
| `nikama help` | コマンド一覧を表示 |

`nikama` の代わりに `digger` も使え、`wakeup_word` で好きな単語にできます。

## Digger にない機能

Digger のバックエンドレスモードでできることに加えて:

- **大きなリポジトリでも結果が早く返る**: プロジェクトを 1 つずつではなく並列に plan します。
- **読みやすい plan / apply 結果**: 実行ごとに 1 件のコメントにまとめ、プロジェクトの完了に合わせて更新します。
- **クラウドリソースが減る**: AWSではロックを S3 バケットにも置けるので、DynamoDB テーブルが不要になります。
- **plan が同僚をブロックしない**: `pr_locks: on_apply` にすると複数の PR が同じプロジェクトを plan でき、ロックを取るのは apply だけになります。
- **依存更新の PR が誰も止めない**: Renovate / Dependabot の PR は、ロックを 1 つも持たずに全ディレクトリを plan します。
- **コメントから対象を絞った plan**: ワークフローを編集せずに、`nikama plan -- ...` で `-target` / `-replace` を許可リストの範囲で渡せます。
- **モジュールの変更を見逃さない**: ローカルモジュールの変更で、それを使うすべてのプロジェクトが plan されます。
- **1 つのディレクトリで複数環境**: プロジェクトの `branch` 正規表現で、staging / production のワークスペースをそれぞれのベースブランチに結びつけます。
- **ドラフトは静かなまま**: ドラフト PR はレビュー準備完了になるまで何も実行せず、ロックも持ちません。
- **古い plan を apply しない**: 保存した plan は生成元のコミットに紐づき、別のコミットでは拒否されます。
- **外部の人に Terraform を実行させない**: コメントコマンドと承認は書き込み権限のあるユーザーのものだけが有効で、`config-ref` / `allowed-providers` により PR がジョブの認証情報で実行される内容を変えられません。

## クイックスタート

必要なのは、ワークフローが OIDC で引き受けられる AWS ロールと、そのロールから
アクセスできる plan ファイル用の S3 バケット、および nikama が初回利用時に作成する
DynamoDB のロックテーブルです。Google Cloud、S3 だけでのロック、必要な権限は
[`docs/CONFIGURATION.ja.md`](docs/CONFIGURATION.ja.md#ロックと-plan-保存のプロバイダ)
を参照してください。

`.github/workflows/nikama.yml`:

```yaml
name: nikama
on:
  pull_request:
    types: [opened, synchronize, reopened, closed, ready_for_review, converted_to_draft]
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
  queue: max

jobs:
  nikama:
    runs-on: ubuntu-latest
    # Skip the runner for comments from outside the repository; nikama checks
    # the commenter's write access itself.
    if: >-
      github.event_name != 'issue_comment'
      || contains(fromJSON('["OWNER", "MEMBER", "COLLABORATOR"]'), github.event.comment.author_association)
    steps:
      - uses: actions/checkout@v7
        with:
          ref: ${{ github.event_name == 'issue_comment' && format('refs/pull/{0}/merge', github.event.issue.number) || '' }}
      - uses: hashicorp/setup-terraform@v4
        with:
          terraform_wrapper: false
      - uses: aws-actions/configure-aws-credentials@v6
        with:
          role-to-assume: arn:aws:iam::123456789012:role/nikama
          aws-region: ap-northeast-1
      - uses: kangaechu/tf-nikama@v0.8.0
        with:
          upload-plan-destination: aws
          upload-plan-destination-s3-bucket: my-plan-bucket
```

リポジトリルートの `nikama.yml`:

```yaml
projects:
  - name: network
    dir: infra/network
  - name: app
    dir: infra/app
    depends_on: [network]
```

`infra/app` を変更するプルリクエストを開くと、上のコメントが投稿されます。

**さらに詳しく**

- [`docs/CONFIGURATION.ja.md`](docs/CONFIGURATION.ja.md): 実行の流れ（ロック、apply の条件、自動マージ）、ロックと plan 保存のプロバイダ、誰が実行を開始でき何を実行してよいか。
- [`docs/PARAMETERS.ja.md`](docs/PARAMETERS.ja.md): アクションの入力、`nikama.yml` のキー、環境変数の一覧とデフォルト値。
- [`examples/nikama.yml`](examples/nikama.yml): 設定スキーマの全体。
- [`docs/MIGRATION.ja.md`](docs/MIGRATION.ja.md): Digger からの移行。

## 対応していないもの

Terragrunt、Pulumi、Azure のロック／plan 保存、GitHub Artifacts への plan 保存、
OPA ポリシー、Digger バックエンドの機能は、設定読み込み時に拒否されます。AWS と
Google Cloud 上の Terraform / OpenTofu 以外に対応する予定はありません。

## コントリビューション

バグ報告と機能要望は
[GitHub Issues](https://github.com/kangaechu/tf-nikama/issues/new) で受け付けて
います。プルリクエストはリポジトリのコラボレーターからのみ受け付け、それ以外の
アカウントからの PR は自動的にクローズされます。[CONTRIBUTING.md](CONTRIBUTING.md)
を参照してください。

## 謝辞

tf-nikama は、Digger HQ による Apache-2.0 ライセンスの
[Digger](https://github.com/diggerhq/digger) のバックエンドレスモードを独立して
再実装したものです。

## ライセンス

Apache License 2.0。[LICENSE](LICENSE) を参照してください。
