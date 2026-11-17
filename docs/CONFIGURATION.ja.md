# 設定リファレンス

[English](CONFIGURATION.md)

実行を形作るものすべてをまとめています。`nikama.yml` のキー、誰が実行を開始でき
何を実行してよいかを決めるアクション入力、そしてバイナリの環境変数と終了コードです。
すべての入力・キー・変数をデフォルト値つきで一覧にしたものは
[PARAMETERS.ja.md](PARAMETERS.ja.md) を参照してください。

- [古い plan コメントの折りたたみ](#古い-plan-コメントの折りたたみ)
- [設定ファイル](#設定ファイル)
- [タイムゾーン](#タイムゾーン)
- [実行の流れ](#実行の流れ)
- [ドラフトのプルリクエスト](#ドラフトのプルリクエスト)
- [PR ロック](#pr-ロック)
- [apply の条件](#apply-の条件)
- [ベースブランチフィルタ](#ベースブランチフィルタ)
- [ローカルモジュール](#ローカルモジュール)
- [ウェイクアップワード](#ウェイクアップワード)
- [コメントでの追加引数](#コメントでの追加引数)
- [ロックと plan 保存のプロバイダ](#ロックと-plan-保存のプロバイダ)
- [自動マージとブランチ保護](#自動マージとブランチ保護)
- [コマンドを実行できる人](#コマンドを実行できる人)
- [設定の信頼](#設定の信頼)
- [プロバイダの制限](#プロバイダの制限)
- [誰から何を守るか](#誰から何を守るか)
- [環境変数](#環境変数)
- [ローカルでの検証](#ローカルでの検証)
- [終了コード](#終了コード)

## 古い plan コメントの折りたたみ

`comment.mode: single`（デフォルト）と `per_project` では、plan の実行が完了し、
最終結果を投稿した後に、古い plan コメントを **outdated** として折りたたみます。
対象は、コメントに記録されたすべてのプロジェクトで、新しい plan が成功した
（`changes` または `no changes` になった）ものです。一部だけを再 plan した場合、
他のプロジェクトの結果が残っているコメントは表示を維持します。複数に分割された
コメントは実行全体の対象プロジェクトで判定し、プロジェクト別のコメントは
それぞれ独立して折りたためます。

折りたたむのは、同じ GitHub アカウントが投稿した、完了済みの古い nikama plan
コメントだけです。apply コメントと実行中の結果は表示を維持します。`latest` は
従来どおり既存のコメントを更新します。対象プロジェクトを記録するメタデータの
導入前に投稿されたコメントも、そのまま表示します。変更のない plan が表から
省略されている可能性があり、対象プロジェクト全体を確認できないためです。

折りたたみには GitHub GraphQL の `minimizeComment` を `OUTDATED` 指定で使います。
コメントは削除されず、展開して確認できます。トークンに折りたたみの権限がない場合は、
警告をログに出し、Terraform の実行結果は変更しません。

## 設定ファイル

リポジトリルートの `nikama.yml`（または `nikama.yaml`）。`digger.yml` も警告付きで
フォールバックとして読み込まれます。未知のキーと未対応機能のキーはエラーになります。
完全な例は [`../examples/nikama.yml`](../examples/nikama.yml) を参照してください。

トップレベルキー: `projects`、`workflows`、`generate_projects`、
`dependency_configuration`、`traverse_to_nested_projects`、`auto_merge`、
`auto_merge_strategy`、`pr_locks`、`allow_draft_prs`、`no_lock_label`、
`wakeup_word`、`autoplan_modules`、`allowed_plan_args`、`timezone`、
`drift_include_patterns`、`drift_exclude_patterns`、`concurrency`、`comment`、
`allowed_providers`（互換性のために `git_timeout` と `telemetry` も受け付けます）。

## タイムゾーン

`timezone` に IANA タイムゾーン名を指定すると、plan・apply などの PR コメントに
表示する実行開始時刻を、指定した地域の時刻に変換できます。

```yaml
timezone: Asia/Tokyo
```

たとえば `2026-01-15 18:30 UTC` は `2026-01-16 03:30 JST` と表示されます。
デフォルトは `UTC` です。無効な名前を指定すると設定の検証でエラーになります。
夏時間のある地域では自動で切り替わります。すべてのコメントモード
（`single`、`per_project`、`latest`）に適用されます。
ロックに保存する時刻や Terraform / OpenTofu の出力は変わりません。

## 実行の流れ

1. イベントは各プロジェクトの `workflow_configuration` に従ってコマンドに対応付け
   られます（デフォルト: push → `digger plan`、クローズ／マージ → `digger unlock`）。
   ドラフトのプルリクエストではデフォルトで何も実行されません（「ドラフトの
   プルリクエスト」参照）。
2. 影響を受けるプロジェクトは PR の変更ファイルから算出されます（`dir/**`、
   `include_patterns`、`exclude_patterns`、および `hard` モードの場合は下流の
   `depends_on` プロジェクト）。`autoplan_modules: true` の場合、プロジェクトが使う
   ローカルモジュールもプロジェクトの一部として扱われます（「ローカルモジュール」
   参照）。`branch` を持つプロジェクトは、ベースブランチが一致するプルリクエストに
   対してのみ対象になります（「ベースブランチフィルタ」参照）。
3. すべてのプロジェクトを *pending* とした実行コメントが即座に 1 件投稿され、
   コミットステータスも *pending* に設定されます。
4. PR ロックは全プロジェクト分を事前に確認し、設定順に取得します（「PR ロック」
   参照）。
5. apply の前に、各プロジェクトの `apply_requirements` が PR に対して確認されます
   （「apply の条件」参照）。
6. plan は並列に実行され（`concurrency.max_parallel`）、apply は依存関係の順に
   レベルごとに実行されます。プロジェクトが完了するたびにコメントが編集されます。
   apply は常に plan ファイルを使います。plan 保存が設定されている場合は保存された
   plan（保存された plan がない場合は、確認なしで apply せずジョブを失敗させます）、
   それ以外の場合は apply の直前に生成した plan を使い、その要約をコメントに
   表示します。保存された plan には生成元の PR ヘッドコミットが記録されており、
   別のコミットの plan は拒否されます。
   変更のない plan は apply しません。そのプロジェクトは *no changes* と報告され、
   Terraform は実行されません（保存された plan は変更の有無を記録しているので、
   判定のために `init` は走りません）。このプロジェクトも `auto_merge` の判定では
   apply 済みとして扱われます。
   apply が失敗したときの残りの扱いは `dependency_configuration.on_failure`
   で決まります（デフォルト `skip_dependents`）。失敗したプロジェクトに直接または
   推移的に依存するプロジェクトは、失敗したプロジェクト名とともに *skipped* と
   報告され、無関係なプロジェクトは後のレベルでも apply されます。`abort` は
   以降のレベルをすべてスキップし（失敗したレベルで実行中の apply は完了まで
   走ります）、`continue` は依存するプロジェクトもそのまま apply します。
   すでに実行された apply がロールバックされることはありません。
7. コミットステータスが success / failure に設定されます。`auto_merge` は
   すべての apply が成功した場合に PR をマージします（ベースブランチが保護されている
   場合は「自動マージとブランチ保護」を参照）。影響するプロジェクトがない
   push は `digger/plan` と `digger/apply` の両方を success にし、変更のある
   プロジェクトがない plan 実行も `digger/apply` を success にします。そのため、
   どちらも apply するものがない PR をブロックすることなく必須チェックとして使えます。
   どちらもコミット全体についての表明なので、影響を受けるプロジェクトの一部しか
   扱わない実行（`-p` / `-d`、または `--` 以降の引数で絞った plan）では success に
   なりません。一部だけの apply では `digger/apply` は pending のままです。
   影響を受けるプロジェクトがあるのにそのイベントで何も実行されなかった場合は、
   どちらのコンテキストも success にはせず、現在の状態のまま残します。

## ドラフトのプルリクエスト

`allow_draft_prs: false`（デフォルト）の場合、ドラフトのプルリクエストでは何も
実行されず、ロックも取得しません。ドラフトへの push は無視され、オープンな
プルリクエストをドラフトに変換するとロックが解放され（`converted_to_draft`）、
`digger unlock` 以外のコメントコマンドはレビュー準備完了にするまで拒否されます。

```yaml
allow_draft_prs: false
```

## PR ロック

PR ロックは全プロジェクト分を事前に確認し、設定順に取得します。いずれかの
プロジェクトを別のオープンな PR が保持している場合、新しいロックは取得しません。
保持されているプロジェクトは *locked by #N*、その他はスキップとして報告され、
この PR がすでに保持しているプロジェクトだけが実行されます。クローズ／マージ時と
引数なしの `digger unlock` では、現在影響を受けるプロジェクトだけでなく、PR が
保持しているすべてのロックが解放されます。別のオープンな PR が保持している
ロックがこの経路で解放されることはありません。クローズされた PR が残したロックは、
そのプロジェクトを必要とする次の PR が引き継ぎます。

ロックを取得するタイミングは `pr_locks` で決まります:

- `on_plan`（デフォルト、`true` も同じ）は最初の plan 時にロックし、1 つの
  プルリクエストが最初の plan からクローズまたはマージまでプロジェクトを保持します。
  変更のない plan は apply するものがないため、run の最後にロックを返します。
- `on_apply` はすべてのプルリクエストが自由に plan でき、`apply` 時にだけロック
  します。その後はクローズ／マージまたは `unlock` までプロジェクトを保持します
  （同じ PR の以降の plan はロックを維持します）。`on_apply` を使う場合は、別の
  プルリクエストがすでに apply してマージした変更を含まない plan を apply できない
  よう、`apply_requirements` に `undiverged` を追加してください。
- `disabled`（`false` も同じ）はロックしません。

```yaml
pr_locks: on_apply
```

plan はロックを完全にスキップすることもできます。コメントで `digger plan --no-lock`
を使うか、`no_lock_label` で指定した名前のラベルが付いたプルリクエストが対象です
（Renovate / Dependabot の設定が付けるラベルを指定してください）。このような plan
は別の PR が保持しているプロジェクトでも実行され、plan 対象プロジェクトについて
この PR が保持しているロックを解放します。そのため、たとえばすべてのディレクトリに
またがる Terraform やプロバイダのバージョン更新のように、plan を確認したいだけの
プルリクエストが誰もブロックしません。`digger apply` は通常どおりロックを取得
します。保存された plan は、引き続き生成元のコミットに紐づきます。

```yaml
no_lock_label: dependencies
```

## apply の条件

apply の前に、各プロジェクトの `apply_requirements` が PR に対して確認されます。
各値は独立して適用されます:

- `mergeable`: GitHub が PR をマージ可能と報告していること。ブランチ保護が
  *blocked* と報告している場合は、必須レビューが揃っていて、nikama 自身のもの以外の
  すべての必須チェックランまたはステータスが成功している必要があります。これにより、
  必須チェックにした `digger/apply` が自分自身をブロックしません。
- `approved`: 作者以外の書き込み権限を持つユーザーによる承認レビューが 1 件以上
  あり、未解決の「changes requested」がないこと。それ以外のユーザーのレビューは、
  ブランチ保護と同様に無視されます。
- `undiverged`: ブランチがベースより遅れていないこと。

デフォルトは `[mergeable]` です。クローズ済みで未マージの PR は常に拒否されます。

```yaml
projects:
  - name: app
    dir: infra/app
    apply_requirements: [approved, mergeable, undiverged]
```

## ベースブランチフィルタ

プロジェクトの `branch` は、プルリクエストのベースブランチ（PR のマージ先であり、
PR 自身のブランチではありません）に対して照合される正規表現です。`branch` が
一致しないプロジェクトは、そのプルリクエストの影響対象になりません。push 時の
plan は行われず、コメントの `-p` / `-d` でも選択されません（理由はコメントで
説明されます）。下流の `depends_on` プロジェクトも同じようにフィルタされます。
同じディレクトリを異なるワークスペースで 2 回登録できます:

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

照合はアンカーなしで行われるため、`main` が `maintenance` にも一致してはいけない
場合は `main` ではなく `^main$` と書いてください。手動実行とドリフト実行には
プルリクエストがないため、このフィルタは無視されます。

## ローカルモジュール

リポジトリ内の別の場所にあるモジュールを使うプロジェクトは、そのモジュールだけが
変更されても影響対象になりません（手動でそのディレクトリを `include_patterns` に
列挙している場合を除く）。`autoplan_modules: true` はこれを自動化します。各
プロジェクトディレクトリの `*.tf` / `*.tofu` ファイルを走査して、`source` が
ローカルパス（`./...` または `../...`）の `module` ブロックを探し、参照先の
ディレクトリを推移的にたどり、その配下のどこかの変更がプロジェクトに影響します。

```yaml
autoplan_modules: true
```

認識されるのは文字列リテラルのソースだけです。レジストリ、git、HTTP のソース、
`*.tf.json` ファイル、チェックアウトに存在しないディレクトリは無視されます。
走査はチェックアウトを読むため、`config-ref` を使っていてもプルリクエスト自身の
モジュール参照をたどりますが、これは影響対象のプロジェクトを広げることしか
できません。`generate_projects` は `modules` ディレクトリをプロジェクトにしない
ため、両者は併用できます。

## ウェイクアップワード

PR コメントは、最初の空でない行がウェイクアップワードとそれに続くコマンドで
始まる場合にコマンドとして扱われます: `nikama plan`、`digger apply -p app`。
デフォルトでは `nikama` と `digger` の両方を受け付けます（大文字小文字を区別
しません）。`wakeup_word` はこのリストを置き換えるので、リポジトリ内の別の bot が
同じ単語に反応する場合や、より短い単語を使いたい場合に設定してください:

```yaml
wakeup_word: tf            # only `tf plan`, `tf apply`, ...
# or several; the first one is used in help texts and comment footers
wakeup_word: [tf, nikama]
```

各単語は空白を含まない 1 トークンである必要があります。`workflow_configuration`
のエントリと手動モードの `command` 入力は、設定した単語とデフォルトの両方を
受け付けるため、そこでは `digger plan` が引き続き動作します。他のキーと同様に、
`config-ref` が設定されている場合 `wakeup_word` は信頼された設定から読み込まれる
ため、プルリクエストがどのコメントで実行が始まるかを変更することはできません。

## コメントでの追加引数

コメントコマンドの `--` 以降はすべて `terraform plan` に渡されます。ワークフローの
plan ステップの `extra_args` の後ろに、環境変数展開なしで付加されます:

```
nikama plan -p app -- -target=module.vpc
nikama plan -p app -- -replace=aws_instance.web
nikama plan -- -refresh=false
```

この機能は `nikama.yml` で渡せるものを列挙するまで無効です。各引数は個別に
パターンと照合され、`*` は任意の文字列に一致します。そのため値は `-flag=value`
の形式で書いてください:

```yaml
allowed_plan_args:
  - "-target=*"
  - "-replace=*"
  - "-refresh=false"
```

リスト外の引数や、リストが空のときの任意の引数はコメントで拒否され、何も
実行されません。他のキーと同様に、`config-ref` が設定されている場合
`allowed_plan_args` は信頼された設定から読み込まれます。state の置き場所や
ロック方法を変えるもの（`-state=`、`-lock=false`、`-backend-config=`）は
含めないでください。

引数は選択されたすべてのプロジェクトに渡されるため、`-target` と `-replace` は
`-p` と組み合わせてください。アドレスが存在しないプロジェクトは、その行で
失敗します。`--no-lock` は `--` と併用できます。

`apply` がこれらをどう引き継ぐかは plan 保存の有無で変わります:

- plan 保存がある場合、保存された plan がすでにターゲット指定を含んでいるため、
  `nikama apply` はそれをそのまま apply します。実行コメントには plan 生成時の
  引数が記載されます。`nikama apply -- ...` は拒否されます。Terraform は
  保存済み plan と plan オプションの併用を受け付けないためです。
- plan 保存がない場合、`apply` は直前に plan を実行するため、同じ引数を
  渡してください: `nikama apply -p app -- -target=module.vpc`。

## ロックと plan 保存のプロバイダ

PR ロックと plan ファイルは自分のクラウドアカウントに置きます。ロックの置き場所は
`lock-provider`、plan の保存先は `upload-plan-destination` で選び、両者は独立して
います。plan 保存は省略でき、その場合 apply は直前に plan を実行します。

### AWS: DynamoDB ロック（デフォルト）

`lock-provider: aws` は、`lock-table` で指定した DynamoDB テーブル（デフォルト
`DiggerDynamoDBLockTable`、Digger と共用）にロックを置きます。テーブルは初回利用時に
nikama が作成するため、ロールにはテーブルに対する `dynamodb:GetItem`、
`dynamodb:Query`、`dynamodb:UpdateItem`、`dynamodb:DeleteItem` に加えて
`dynamodb:DescribeTable` と `dynamodb:CreateTable` が必要です。`CreateTable` を
付与したくない場合は、パーティションキー `PK`（文字列）、ソートキー `SK`（文字列）、
オンデマンドキャパシティでテーブルを事前に作成してください。

```yaml
      - uses: kangaechu/tf-nikama@v0.7.0
        with:
          upload-plan-destination: aws
          upload-plan-destination-s3-bucket: my-plan-bucket
```

### AWS: S3 ロック

`lock-provider: s3` はすべてを S3 に置き、DynamoDB の権限を必要としません。ロック
オブジェクトは `If-None-Match: *` 付きで作成されるため、2 つの実行が同じ
プロジェクトを同時に取得することはありません。ロールにはロックバケットに対する
`s3:PutObject`、`s3:GetObject`、`s3:DeleteObject`、`s3:ListBucket` が必要です。
ロックバケットは plan バケットと兼用できます。

```yaml
      - uses: kangaechu/tf-nikama@v0.7.0
        with:
          lock-provider: s3
          aws-lock-bucket: my-lock-bucket
          upload-plan-destination: aws
          upload-plan-destination-s3-bucket: my-plan-bucket
```

### Google Cloud: Cloud Storage

AWS の認証ステップを `google-github-actions/auth` に置き換え、アクションの向き先を
Cloud Storage のバケットにします。サービスアカウントには両方のバケットに対する
`roles/storage.objectUser` が必要です。ロックオブジェクトと plan ファイルは Digger の
`gcp` プロバイダと互換性があります。

```yaml
      - uses: google-github-actions/auth@v3
        with:
          workload_identity_provider: projects/123456789012/locations/global/workloadIdentityPools/github/providers/github
          service_account: nikama@my-project.iam.gserviceaccount.com
      - uses: kangaechu/tf-nikama@v0.7.0
        with:
          lock-provider: gcp
          google-lock-bucket: my-lock-bucket
          upload-plan-destination: gcp
          upload-plan-destination-gcp-bucket: my-plan-bucket
```

### ロックなし

`lock-provider: noop`（または `nikama.yml` の `pr_locks: disabled`）はロックなしで
実行します。2 つのプルリクエストが同じ state に触れることが決してない場合にだけ
使ってください。

## 自動マージとブランチ保護

`auto_merge` は、アクションが実行に使っているトークンで PR をマージします。
デフォルトではワークフローの `GITHUB_TOKEN` なので、マージは `github-actions[bot]`
によって行われ、ベースブランチの次の 2 つのブランチ保護設定がこれを拒否します:

- **Restrict who can push to matching branches**（または push やバイパスできる人を
  制限するルールセット）。マージは push として扱われ、`GITHUB_TOKEN` の背後にある
  GitHub Actions のアイデンティティはこのリストに追加できません。GitHub は
  `405` と "You're not authorized to push to this branch" を返し、ログには
  `auto-merge: merge failed` が出ます。`permissions:` を増やしても、"Allow
  specified actors to bypass required pull requests" を設定してもこれは変わりません。
- **Require status checks to pass** で、nikama のワークフロージョブ自体を必須
  チェックにしている場合。ジョブはマージ前に完了できないため、GitHub は
  "Required status check ... is expected" を返します。代わりに `digger/apply`
  コミットステータスを必須にしてください。nikama はマージ前にこれを success に
  設定します。

1 つ目のケースの回避策は、許可リストに追加できる GitHub App としてマージすることです:

1. GitHub App を作成します（Organization またはユーザー設定 → Developer settings）。
   リポジトリ権限は **Contents: read and write**、**Pull requests: read and
   write**、**Commit statuses: read and write**、さらに `drift-github-issues: true`
   でドリフト検知を使う場合は **Issues: read and write** が必要です。Webhook は
   不要です。
2. アプリをリポジトリにインストールし、ベースブランチの "Restrict who can push
   to matching branches"（ブランチ保護）またはバイパスリスト（ルールセット）に
   追加します。
3. App ID と秘密鍵をリポジトリシークレットに保存し、ジョブの先頭でインストール
   トークンを発行します。それを `github-token` として渡してください。nikama は
   マージだけでなく、すべての GitHub API 呼び出しにこのトークンを使います。

```yaml
    steps:
      - uses: actions/create-github-app-token@v3
        id: app-token
        with:
          app-id: ${{ secrets.NIKAMA_APP_ID }}
          private-key: ${{ secrets.NIKAMA_APP_PRIVATE_KEY }}
      - uses: actions/checkout@v7
        # ...
      - uses: kangaechu/tf-nikama@v0.7.0
        with:
          github-token: ${{ steps.app-token.outputs.token }}
          # ...
```

この場合、ワークフローの `permissions:` ブロックは `id-token: write`（クラウド
認証）にだけ関係します。アプリトークンは自身の権限を持っています。`GITHUB_TOKEN`
と違い、アプリトークンによるマージはベースブランチの `push` ワークフローを
トリガーするため、マージ後に apply するワークフローも通常どおり動きます。

許可リストに含まれるユーザーの personal access token でも同じように動きますが、
マージがその人のアカウントに紐づきます。

## コマンドを実行できる人

実行はプルリクエストの `nikama.yml` と Terraform コード（プロバイダを含む）を
ワークフローの認証情報で実行するため、実行を開始することはリポジトリ内でコードを
実行することと同等です。そのため、コメントコマンド（`nikama plan`、`nikama apply`
など）はリポジトリロールが `write`、`maintain`、`admin` のユーザーからのみ
受け付けられ、実行時にコラボレーター権限 API で確認されます。それ以外の人からの
コマンドには短いコメントで返答し、ワークフローは 0 で終了します。`nikama help`
には誰に対しても応答します。

README のクイックスタートの `if:` ガードは、リポジトリ外からのコメントでランナーを起動
しないための最適化にすぎません。`COLLABORATOR` と `MEMBER` には読み取り専用の
ユーザーも含まれるため、本当に効いているのは nikama 内部の権限チェックです。

`pull_request` イベントにはこのような確認は不要です。GitHub はフォークからの
プルリクエストでトリガーされたワークフローに読み取り専用トークンを渡し、OIDC
トークンを渡しません。また、リポジトリ自身のブランチには書き込み権限のある
ユーザーしか push できません。

## 設定の信頼

デフォルトでは `nikama.yml` はチェックアウトから読み込まれ、プルリクエストの実行では
プルリクエスト自身のバージョンになります。次の 2 つの入力でこれを厳しくできます:

- `config-ref: default` は、リポジトリのデフォルトブランチ（または任意の git ref）
  から GitHub API 経由で設定を読み込みます。これにより、プルリクエストはレビュー済みの
  設定で実行されます。`run:` ステップ、ワークフロー、`apply_requirements`、
  `aws_role_to_assume` は PR 自身からは変更できません。`nikama.yml` の変更は
  マージ後に有効になり、新しいプロジェクトを追加する PR はマージ後に plan されます。
  `generate_projects` は引き続きチェックアウト内のディレクトリを探索します。この
  入力はプルリクエストの実行にのみ適用され、手動実行とドリフト実行はチェックアウトを
  読みます。
- `allow-run-steps: false` は、どこから読み込んだ設定であっても `run:` ステップを
  含むものを拒否します。

```yaml
      - uses: kangaechu/tf-nikama@v0.7.0
        with:
          config-ref: default
          allow-run-steps: "false"
```

## プロバイダの制限

`terraform plan` はプロバイダのバイナリを実行するため、プロバイダを追加する
プルリクエスト（`hashicorp/external` や、別のレジストリ・名前空間のもの）は
ジョブの認証情報で任意のコードを実行できてしまいます。`allowed-providers` で
インストールできるものを制限します:

```yaml
      - uses: kangaechu/tf-nikama@v0.7.0
        with:
          allowed-providers: "hashicorp/aws, hashicorp/random, app.terraform.io/myorg/*"
```

エントリはプロバイダのソースアドレスで、`namespace/type` は
`registry.terraform.io/namespace/type` を意味し、`*` は任意のセグメント全体を
置き換えられます。nikama はこのリストを Terraform CLI 設定（`direct` メソッドと
その `include` リストを持つ `provider_installation`）に変換し、
`TF_CLI_CONFIG_FILE` を通じてすべての `terraform` / `tofu` 呼び出しに渡します。
これにより `init` は、子モジュールが要求するプロバイダも含め、リスト外の
プロバイダを何もダウンロード・実行する前に拒否します。組み込みプロバイダ
（`terraform_data`）には影響しません。

- `nikama.yml` にもトップレベルの `allowed_providers:` リストを置けます。これは
  ワークフロー入力を狭めることしかできません。`allowed-providers` に含まれない
  エントリは警告付きで無視され、何も残らないリストはエラーになります。ワークフロー
  入力がない場合は `nikama.yml` のリストが単独で適用されますが、これは `config-ref`
  と組み合わせた場合にのみ意味があります。
- 既存の CLI 設定（`TF_CLI_CONFIG_FILE`、`~/.terraformrc`、`~/.tofurc`）は維持され、
  拡張されます。すでに `provider_installation` ブロック（ファイルシステムまたは
  ネットワークミラー）がある場合は、そちらに `include` リストを書き、
  `allowed-providers` は未設定にしてください。`TF_PLUGIN_CACHE_DIR` は引き続き
  動作します。

## 誰から何を守るか

| トリガー | PR 作者 | 作者のコードを実行するもの | 保護 |
|---|---|---|---|
| `issue_comment` | 誰でも（フォーク、読み取り専用） | なし: コマンドには書き込み権限が必要 | 書き込み権限チェック（組み込み） |
| 信頼できない PR に対するメンテナの `issue_comment` | フォーク、読み取り専用 | PR の Terraform コード。`config-ref` がなければその `nikama.yml` も | `config-ref`、`allow-run-steps`、`allowed-providers` |
| `pull_request` | フォーク | 実質なし: 読み取り専用トークン、OIDC トークンなし、シークレットなし | GitHub |
| `pull_request` | 同一リポジトリの書き込み権限者 | PR のワークフローファイル、つまりすべて | nikama では解決不可: 書き込み権限を信頼するか、`on.pull_request` をやめてブランチ保護付きのコメントコマンドを使うか、保護された GitHub Environment でクラウドロールを制限する |

コメントのみでトリガーする構成では、`config-ref: default` と `allowed-providers`
は書き込み権限のあるユーザーに対しても有効です。どちらを変更するにもデフォルト
ブランチへのマージが必要になるためです。

## 環境変数

アクションはこれらを自動で設定します。バイナリを直接実行する場合:

| 変数 | 用途 |
|---|---|
| `GITHUB_TOKEN`、`GITHUB_CONTEXT`、`GITHUB_REPOSITORY` | PR モードで必須 |
| `INPUT_DIGGER_MODE` | `` / `manual` / `drift-detection` |
| `INPUT_DIGGER_COMMAND`、`INPUT_DIGGER_PROJECT` | 手動モード |
| `LOCK_PROVIDER`（`aws`/`s3`/`gcp`/`noop`）、`NIKAMA_LOCK_TABLE`、`AWS_S3_LOCK_BUCKET`、`GOOGLE_STORAGE_LOCK_BUCKET`、`DISABLE_LOCKING` | PR ロック |
| `PLAN_UPLOAD_DESTINATION`（`aws`/`gcp`）、`AWS_S3_BUCKET`、`PLAN_UPLOAD_S3_*`、`GOOGLE_STORAGE_PLAN_ARTEFACT_BUCKET` | plan 保存 |
| `REPORTING_STRATEGY` | `comments_per_run` / `latest_run_comment` / `per_project` |
| `INPUT_DRIFT_DETECTION_SLACK_NOTIFICATION_URL`、`INPUT_DRIFT_GITHUB_ISSUES` | ドリフト通知 |
| `NIKAMA_CONFIG`、`NIKAMA_MAX_PARALLEL`、`NIKAMA_LOG_LEVEL` | CLI オプション |
| `NIKAMA_CONFIG_REF`、`NIKAMA_ALLOW_RUN_STEPS` | 信頼された設定（「設定の信頼」参照） |
| `NIKAMA_ALLOWED_PROVIDERS` | プロバイダ許可リスト（「プロバイダの制限」参照） |
| `TF_PLUGIN_CACHE_DIR` | 設定時、`init` ステップは 1 つずつ実行される |

`NIKAMA_*` 名と Digger 互換名の両方がある変数は `NIKAMA_*` が優先されます。一覧は [PARAMETERS.ja.md](PARAMETERS.ja.md#バイナリ-フラグと環境変数) を参照してください。

## ローカルでの検証

```sh
nikama --validate --local
```

## 終了コード

`0` 正常／何もすることがない、`1` 設定エラー、`2` ロックプロバイダのエラー、
`3` コンテキスト解析エラー、`4` 初期化エラー、`6` イベントエラー、
`7` コマンドエラー、`8` 1 つ以上のジョブが失敗、`10` GitHub Actions 外で実行。
アクションが扱わないイベント（たとえば PR モードに届いた `push` や
`workflow_run`）は無視され、終了コード `0` になります。
