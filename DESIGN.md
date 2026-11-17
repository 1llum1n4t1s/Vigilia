# DESIGN.md

Vigilia の設計。使い方は [README.md](README.md)、開発規約は [AGENTS.md](AGENTS.md) を参照する。

## 目的

複数の Claude Code セッションが同じ GitHub アカウントで `gh run watch` などを polling すると、認証ユーザー単位で共有される REST の枠（5,000 回/時）を使い切り、`HTTP 403: API rate limit exceeded` が頻発する。待つだけでは総処理量が増えないため、Vigilia は次の2点で REST 枠への依存をなくす。

1. **取得を GraphQL に移す。** GraphQL の枠は REST と別に数えられる。`checkSuites` / `workflowRun` / `checkRuns` を1クエリにまとめ、1回あたりのコストは 1〜3 ポイント程度。
2. **取得結果をセッション間で共有する。** 同じ対象を複数のプロセスが見ていても、TTL 内は GitHub へ問い合わせない。

非目的: 常駐デーモン、通知、REST 枠が使える前提の機能、GitHub Actions 以外の CI。

## 構成と責務

```text
main.rs ─ clap 引数 → commands.rs (watch / status / runs / budget)
                          │
                          ├─ target.rs   対象と repo の解釈（run id / SHA / HEAD / ブランチ(runs のみ)、git remote 推定）
                          ├─ service.rs  Resolver: キャッシュ → GraphQL の取得を1か所に集約
                          │     ├─ cache.rs   ファイルキャッシュ + ロック
                          │     └─ github.rs  GraphQL / rate_limit クライアント、トークン、HTTP エラー分類
                          ├─ model.rs    Snapshot・結論 (Overall) の判定、GraphQL 応答の正規化
                          └─ render.rs   表示整形
error.rs: VigiliaError と終了コード（全層で共有）
```

- `commands.rs` はループ・待機・出力・終了コードの決定だけを持ち、取得の詳細は知らない。
- `service.rs` の `Resolver` が取得の唯一の入口。`watch` と `status` が同じ経路を通るので、キャッシュ共有が両方で効く。`budget` だけは `Client` を直接使う（キャッシュ対象外）。
- `github.rs` は HTTP と GraphQL に閉じ、ドメイン判断をしない。応答の正規化と結論の判定は `model.rs`。

## データフロー

1. `target.rs` が引数を `Target::Run(id)` か `Target::Sha(sha)` に分類する。数字のみ9桁以上は run id、16進7〜40桁は SHA。`HEAD`・省略は `git rev-parse HEAD`。SHA のときの repo は `--repo`、なければ git remote（`origin` 優先、github.com のみ）から推定する。run id には repo を要求しない。
2. `Resolver::get()` がキャッシュキー（`run__<id>` または `<owner>__<repo>__sha__<sha>`）で `cache.get_or_lock()` を呼ぶ。
   - 新鮮なエントリがあれば返す（`Source::Cache`）。
   - なければロックを取り、ダブルチェックしたうえで GraphQL を1回実行し、結果を書き込む（`Source::Api`）。
   - 他セッションがロック保持中なら最大8秒、更新を待つ。それでも更新されなければ自分で取得する。
3. GraphQL 応答は `model.rs` で `Snapshot`（suite → job の正規化済みツリー）に変換される。キャッシュ・`--json` 出力は `Snapshot` をそのまま使う。
4. `Snapshot::overall()` が全 suite を畳み込んで結論を出し、`commands.rs` が終了コードに変換する。
5. `watch` は `--interval` ごとに 1〜4 を繰り返し、状態が変わった job だけを出力し、結論が確定したら終了する。

クエリは2種類。SHA 起点は `repository.object(expression)` から `checkSuites(first:50)` と各 `checkRuns(first:30)`、run id 起点は `node(id)` から `checkSuite` と `checkRuns(first:50)`。どちらも各 suite から `branch{name}`、`workflowRun{databaseId runNumber runAttempt createdAt event url workflow{name} file{path}}` を取る（`runs` と `--workflow` に必要な分。追加は connection ではなくスカラー / 単一オブジェクトなので cost は 1 のまま）。

### `runs` と `--workflow` / `--event`

追加クエリは作らず、既存の SHA 起点クエリ・キャッシュ・待機の仕組みをそのまま使う。
- **ブランチ指定**は `object(expression: "refs/heads/<名前>")` で先頭コミットへ解決する（同名タグとの衝突を避けるため完全な ref 名にする）。`Target::Branch` はこのクエリ用で、`watch` / `status` は受け付けない。
- **絞り込みは取得の後**に `Snapshot` へ適用する（`model.rs` の `RunFilter` / `Snapshot::filtered()` / `Snapshot::runs()`）。キャッシュのキーに条件を含めないので、条件違いのセッションが同じ取得結果を安全に共有できる。ブランチのキーだけは、ファイル名で `/` と `_` が潰れて衝突しないようハッシュを添える。
- **完了済みの再利用は `Freshness` で決める。** 絞る前の全体が完了済みでも、待っている run が未出現なだけの場合があるため、`Filtered` は「絞った後の結論が確定したとき」だけ `TERMINAL_TTL` を適用する。`runs`（`Strict`）は新しい run・ブランチ先頭の移動を拾うため `TERMINAL_TTL` を適用しない。
- **複数一致の規則**（`Snapshot::filtered`）: 指定が複数の別 workflow に一致したら使用エラー（終了コード 2）。同じ (workflow, event) の run は最新の 1 件だけを結論に使う（再実行・重複 push で古い失敗が新しい成功を隠さない）。event が違う run は別物として全て含める。`runs` は一致した run を全て列挙する。
- **workflow の一致**は、表示名・ファイルパス・ファイル名のいずれかと完全一致（大文字小文字を区別）。動的 workflow（Dependabot Updates など）はパスを持たないので表示名だけで指定できる。
- **`runs --wait`** は `watch` と同じ最小間隔（5 秒）・レート制限待機・古いキャッシュ時の待機を使い、現れなければ終了コード 7。`runs` は結論を判定せず、run が 1 件でもあれば 0。

## 不変条件

- **REST の枠を消費しない。** 使う HTTP は `POST /graphql` と、枠を消費しない `GET /rate_limit` だけ。REST の枠が 0 でも動く。
- **リクエストは直列で、`watch` と `runs --wait` の間隔は 5 秒以上。** 二次レート制限（短時間の同時接続・過剰頻度）を避ける。
- **トークンを出さない。** `GH_TOKEN` / `GITHUB_TOKEN`、なければ `gh auth token`。メモリ内のみで保持し、ログ・エラー・出力・キャッシュに書かない。reqwest のエラーは URL を除いて表示する。
- **結論の規則は1か所。** 未完了の suite が1つでもあれば実行中、全完了なら 失敗 > 取消 > 成功。失敗は FAILURE / TIMED_OUT / STARTUP_FAILURE / ACTION_REQUIRED、取消は CANCELLED / STALE。スキップ・中立は成功扱い。
- **切り捨てた状態で成功と断定しない。** suite が上限（50）を超えて切り捨てられ、かつ結論が「成功」のときは、取りこぼした suite が失敗している可能性があるためエラー（終了コード 2）にする。job の切り捨ては表示上の注意に留める（suite の結論は GitHub が集計済みのため）。
- **キャッシュは最適化であり正本ではない。** 読めない・壊れた・版違いのエントリは「なし」として再取得する。ロックは助言的で、壊れても「重複取得」になるだけで正しさに影響しない。
- **終了コードは機械可読な契約。** 0 成功 / 1 失敗 / 2 入力・認証・通信・API エラー / 3 取消 / 4 timeout / 5 レート制限 / 6 実行中（`status`） / 7 対象なし。定義は `error.rs`、利用者向けの表は README。

## 採用した設計判断

### 常駐デーモンを持たず、ファイルで協調する
キャッシュは1キー1ファイル（`<key>.json`）。書き込みは同一ディレクトリの一時ファイルへ書いてから rename し、読み手は旧版か新版のどちらかを必ず読む。重複取得を避けるため、キーごとの `.lock`（`create_new`）を使い、60秒を超えたロックは古いとみなして奪う。
- 理由: セッションは別プロセスで寿命も異なり、デーモンの起動管理は運用負担になる。ファイルだけなら依存も障害点も増えない。
- トレードオフ: ロックは厳密な排他ではない。Windows では rename 置換の瞬間に読み取りが共有違反で失敗し得るが、その場合は「キャッシュなし」として扱う。最悪でも取得が重複するだけ。

### TTL とキャッシュ寿命
`--ttl`（既定15秒）以内なら新鮮。結論が確定したスナップショットは最大5分（`TERMINAL_TTL`）まで再利用する。
- 理由: 完了後も `watch` / `status` が繰り返し呼ばれるが、re-run で状態が変わり得るため無期限にはしない。

### レート制限時は落とさず待つ
`Retry-After`、残り0のときの `x-ratelimit-reset`、GraphQL の `RATE_LIMITED`（HTTP 200 で返る）から待ち時間を求める。分からなければ60秒。`watch` はその間（`--interval` を下回らない）API を叩かず待って再開し、`--timeout` を超えそうなら終了コード 5 で返す。制限中に古いキャッシュがあれば、警告付きでそれを返して継続する（`Source::StaleCache`、待ち時間は呼び出し側へ渡す）。`status` は待たず終了コード 5。通信エラーと 5xx は最大3回、指数バックオフ（1秒・2秒）で再試行する。
- 理由: 長時間の監視が一時的な制限で途切れると、呼び出し側の AI エージェントが誤判断する。

### run id の解決にレガシー形式のグローバル ID を使う
run id から GraphQL の `node(id:)` を引くため、`base64("011:WorkflowRun<id>")` を組み立てる（`github.rs` の `legacy_run_node_id`）。
- 理由: REST を使わずに run id から run を引ける経路がこれしかない。
- トレードオフ: GitHub が非推奨とする形式。廃止されたら run id 指定だけが API エラーになる。SHA 指定は影響を受けない。

### 監視対象の suite を絞る
check run を持たず workflow run にも紐づかない suite（Actions 以外のアプリが残す、完了しない suite）は除外する（`model.rs` の `keep_relevant`）。
- 理由: 常に QUEUED のまま残る suite があると、`watch` が永遠に完了しない。
- トレードオフ: Actions 以外のチェックは結論に含まれない。

### 生レスポンスと正規化済みモデルを分離する
GraphQL の `Raw*` 型と `Snapshot` を分け、キャッシュ形式・JSON 出力を API の形から切り離す。形式を非互換に変えるときは `ENTRY_VERSION` を上げ、古いエントリは無視される。

### base64 を自前実装する
`legacy_run_node_id` 用に標準アルファベットの最小実装を持ち、依存を増やさない。用途は固定長の短い文字列のみ。

### reqwest は blocking + rustls
リクエストが直列でループが単純なため、非同期ランタイムを持ち込まない。TLS は rustls で OS 依存を減らす。

## 配布

利用者は CI も Rust も不要で使えるよう、ビルド済みバイナリを Cloudflare R2 から配る（運用手順は AGENTS.md の「リリースと配信」）。

- **R2 だけを入口にする。** Git タグ・GitHub Release は使わない。`release/<版>` ブランチの push で `release.yml` が動く。本人の他製品（Sigilune・Skilvella など）と同じ方式で、配信の転送実装（`scripts/R2Transfer.Core.cs`）も共通の同梱コピーを使う。
- **版付きは不変、最後に `latest.json`。** 版付きのアーカイブと `.sha256` を新規作成だけで置き、公開 URL から取り直して照合してからフィードを進める。途中で失敗しても、利用者が見る `latest.json` は古い完全な版のまま残る。
- **`latest.json` は複数ターゲットを表す。** 導入スクリプトは版とターゲットからアーカイブのパスを自分で組み立て、フィードの値は照合にだけ使う（任意のパスへ誘導されない）。
- **導入スクリプトは検証してから置く。** どちらも https のみ・リダイレクトを辿らない・SHA-256 照合。Windows は Authenticode 署名者の CN も照合する。macOS は公証なしの ad-hoc 署名で、導入時は署名の整合だけ確かめる。
- **Secrets を持つ job を最小にする。** ビルドと梱包は Secrets なしの job で行い、署名と配信だけを Environment `release` の job に置く。

## 既知の制約

- 1コミットあたり check suite 50件、1 suite あたり check run 30件（run id 起点は50件）を超える分は取得しない。
- 配布するのは Windows x64（主）、macOS Apple Silicon、Linux x64 / arm64（glibc）。Intel Mac と Windows arm64 は対象外。Linux のキャッシュ先は `$XDG_CACHE_HOME`。
- github.com のみ（GitHub Enterprise のホストは扱わない）。
