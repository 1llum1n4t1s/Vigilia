[English](README.md) | **日本語**

# Vigilia

Vigilia(ラテン語で「見張り」)は、**GitHub REST の API 枠を使わずに** GitHub Actions の状態を見張る小さな CLI です。
CI の状態取得を別枠の **GraphQL** で行い、取得結果を **ファイルキャッシュで複数セッション間に共有**します。Published by Kagayoi.

## なぜ作ったか

複数の AI エージェントのセッション(やスクリプト)が同じ GitHub アカウントで `gh run watch` や `gh run view` を polling すると、
**認証ユーザー単位で共有される REST の枠(5,000 回/時)** を全セッションが食い潰し、
`HTTP 403: API rate limit exceeded` が頻発します。待つだけでは総速度が上がらないため、次の 2 点で対処します。

1. **GraphQL で取得する**: GraphQL の枠(5,000 ポイント/時)は REST と別に数えられ、ほぼ未使用です。
   `checkSuites` / `workflowRun` / `checkRuns` を 1 クエリにまとめ、コストは 1 回あたり 1〜3 ポイントほどです。
   REST の枠が 0 でも動作します(使うのは `POST /graphql` と、枠を消費しない `GET /rate_limit` だけ)。
2. **結果を共有する**: 取得結果をローカルのキャッシュに保存し、同じ対象を別セッションが見ていても
   TTL(既定 15 秒)内は再取得せずキャッシュを返します。同時に取りに行く場合も、軽量ロックで 1 回に集約します。
   常駐デーモンは持ちません(ファイルだけで協調します)。

内部構成と設計判断は [DESIGN.md](DESIGN.md)、開発者向けの規約は [AGENTS.md](AGENTS.md) を参照してください。

## インストール

ビルド済みバイナリを https://vigilia.kagayoi.com(Cloudflare R2)で配布しています。ダウンロードは SHA-256 のチェックサムで照合し、Windows の実行ファイルは Authenticode 署名の署名者も検証します。

Windows x64(PowerShell):

```powershell
irm https://raw.githubusercontent.com/1llum1n4t1s/Vigilia/main/install.ps1 | iex
```

macOS(Apple Silicon)と Linux(x86_64 / aarch64。glibc ベースのディストリビューション、ubuntu-24.04 でビルド):

```sh
curl -fsSL https://raw.githubusercontent.com/1llum1n4t1s/Vigilia/main/install.sh | sh
```

どちらも最新版をインストールします。シェルで版を指定するには `sh -s -- --version 1.0.1` を付けます。`| iex` には引数を渡せないため、PowerShell では script block 形式にします。

```powershell
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/1llum1n4t1s/Vigilia/main/install.ps1))) -Version 1.0.1
```

`install.sh` は `vigilia` を `~/.local/bin` に置き(`--install-dir` または `VIGILIA_INSTALL_DIR` で変更可)、PATH に無ければ案内を表示します。

アーカイブを直接ダウンロードすることもできます。`latest.json` に最新の版と、ターゲットごとのアーカイブ・チェックサムのファイル・SHA-256 が載っています。

| ターゲット | アーカイブ |
| --- | --- |
| Windows x64 | `<版>/vigilia-<版>-x86_64-pc-windows-msvc.zip` |
| macOS Apple Silicon | `<版>/vigilia-<版>-aarch64-apple-darwin.tar.gz` |
| Linux x86_64 | `<版>/vigilia-<版>-x86_64-unknown-linux-gnu.tar.gz` |
| Linux aarch64 | `<版>/vigilia-<版>-aarch64-unknown-linux-gnu.tar.gz` |

各アーカイブの隣に `.sha256`(`sha256sum` 形式)があります。macOS のバイナリは ad-hoc 署名で、公証はしていません。

ソースからビルドする場合は、Rust(edition 2024 対応の stable)でインストールします。

```sh
cargo install --git https://github.com/1llum1n4t1s/Vigilia
```

または、リポジトリを取得してビルドします。

```sh
cargo build --release   # target/release/vigilia(Windows は vigilia.exe)
```

CI は Windows x64、macOS Apple Silicon、Linux でビルド・テスト・lint を行っています。Intel Mac は対象外です。

認証は既存の GitHub CLI のログインを借ります(`gh auth token`)。`gh` のインストールとサインインが必要です。
環境変数 `GH_TOKEN` / `GITHUB_TOKEN` があればそれを優先します。トークンはログ・出力・キャッシュのいずれにも書き出しません。

## クイックスタート

```sh
git push
vigilia watch HEAD      # HEAD の全ワークフローが終わるまで待つ
vigilia status HEAD     # 現在の状態を 1 回表示する
vigilia runs HEAD       # workflow run の一覧を表示する
vigilia budget          # REST / GraphQL / search の残り枠を表示する
```

出力例:

```text
$ vigilia watch e8c8bb3
1llum1n4t1s/CleanReload @ e8c8bb3 を監視します
[21:57:42] CI #7 (push) : 実行中
[21:57:42]   build : 成功
[21:57:42]   test : 実行中
[21:58:22]   test : 失敗
失敗: CI #7 (push) / test [失敗] https://github.com/.../job/123
結論: 失敗 (1llum1n4t1s/CleanReload @ e8c8bb3)
```

`watch` は状態が変わった job だけを出力します。`status` は全 job を 1 回表示し、`--json` で機械可読にできます(`overall` / `source` / `age_secs` / `filter` / `snapshot`)。レート制限中に古いキャッシュしか得られないときは、見出しが結論ではなく「結論: 未確定(レート制限中の古い断面: …)」になり、`--json` は `overall` が null になって、古い断面から計算した値は追加フィールド `stale_overall` に入ります(`source` は `stale-cache`、終了コードは 5)。

## コマンド

```text
vigilia watch  [<run-id | sha | HEAD>] [--repo OWNER/REPO] [--workflow <名前|ファイル>] [--event <イベント>] [--interval 20] [--timeout 3600] [--wait-for-runs 120] [--ttl 15] [--no-cache]
vigilia status [<run-id | sha | HEAD>] [--repo OWNER/REPO] [--workflow <名前|ファイル>] [--event <イベント>] [--json] [--ttl 15] [--no-cache]
vigilia runs   [<sha | HEAD | ブランチ>] [--repo OWNER/REPO] [--workflow <名前|ファイル>] [--event <イベント>] [--since <RFC3339>] [--wait <秒>] [--interval 10] [--json] [--ttl 15] [--no-cache]
vigilia budget [--json]
```

- 対象は **run id**(数字 9 桁以上)または **コミット SHA**(16 進 7〜40 桁、短縮可)。省略または `HEAD` でカレントの `HEAD`。
- `--repo` を省略するとカレントの git remote(`origin` 優先、github.com のみ。ポート 443 の SSH 用 `ssh.github.com` は github.com として扱い、GitHub Enterprise のホストは扱いません)から推定します。run id のときは不要です。
- SHA を指定すると、そのコミットの全 check suite(ワークフロー)をまとめて監視し、すべて完了した時点で結論を出します。
  複数 suite の結論は 失敗 > 取消 > 成功 の優先順です。
- `budget` は REST / GraphQL / search の残り枠とリセット時刻を、枠を消費せずに表示します。

### 特定の workflow だけを見る(`--workflow` / `--event`)

SHA 指定は既定でそのコミットの**全 workflow**(Dependabot の run も含む)を待ちます。`watch` / `status` に `--workflow` を付けると、その workflow の run だけで結論を判定します。`--event`(`push` / `workflow_dispatch` など)でさらに絞れます。SHA / `HEAD` 指定のときだけ使え、run id との併用は使用エラーです。

- `--workflow` は**表示名**(`Release`)、**ファイルパス**(`.github/workflows/release.yml`)、**ファイル名**(`release.yml`)のどれでも指定できます。一致は完全一致で大文字小文字を区別し、部分一致はしません。`--event` は大文字小文字を区別しません。
- 一致する run が現れるまで、`watch` は待機し(`--wait-for-runs` 秒まで。超えたら終了コード 7)、`status` は終了コード 7 で返します。
- **複数の run が一致する場合**: (workflow, event) の組ごとに**最も新しい run だけ**を結論に使います(作成時刻、同時刻なら run 番号の大きい方)。新しい run に取って代わられた古い失敗は、新しい成功を隠しません。**event が違う run**(同じ workflow の `push` と `workflow_dispatch` など)は別の組として**すべて**結論に含め、失敗 > 取消 > 成功の優先順で畳み込みます。片方だけ見たいときは `--event` を併用します。再実行は run id が同じまま attempt が増えるので 1 つの run です。
- **曖昧な指定**: 指定値が複数の別 workflow に一致する場合(例: 表示名 `CI` を持つファイルが 2 つ)は、`watch` / `status` は候補を挙げて終了コード 2 で止まります。ファイルパスで指定してください。
- Dependabot Updates のような動的 workflow はファイルパスを持たないため、表示名でだけ指定できます。
- キャッシュは「コミット全体」を保存し、絞り込み条件に依存しません。条件違いのセッション同士でも安全に共有できます。キャッシュの再利用は完了済みかどうかによらず `--ttl`(既定 15 秒)以内だけなので、まだ現れていない workflow や再実行(re-run)後の結果を、それより古いスナップショットで見逃すことはありません(直後から確実に見るには `--no-cache`)。ただしレート制限中は例外で、古いキャッシュで代替し得ます(「レート制限への振る舞い」の節を参照)。

### run 一覧(`runs`)

`vigilia runs` は、コミットの workflow run 一覧を GraphQL だけで取得します。`gh run list --commit ... --json ...`(REST を消費する)の代わりに使えます。対象は SHA・`HEAD`・**ブランチ名**です(ブランチは同じクエリで `refs/heads/<名前>` として先頭コミットへ解決します。run id は指定できません)。

```text
$ vigilia runs abeebf669a1fbb239d7bf537c88293d0b98177af --repo 1llum1n4t1s/Vigilia --workflow release.yml
1llum1n4t1s/Vigilia @ abeebf6  run 1 件
  44146220753  成功  Release [.github/workflows/release.yml]  push  2026-11-17T18:17:24Z  release/1.0.1  https://github.com/.../runs/44146220753
```

- 絞り込み: `--workflow`(一致規則は上と同じ。複数の workflow に一致してもエラーにせず全て出す)、`--event`、`--since <RFC 3339>`(その時刻以後に作成された run。作成時刻が不明な run は除外)。
- `--wait <秒>` は、条件に合う run が現れるまで最大その秒数待ちます(既定 0 は 1 回だけ取得)。`--interval` 秒ごと(既定 10、5 以上)に取得し、レート制限の待機とキャッシュ共有は `watch` と同じ仕組みです。現れなければ終了コード 7(`--json` では `[]` を出力)。ただし check suite が切り捨てられていて一致が 0 件の場合は「無い」と断定できないため、待たずに即、終了コード 2 のエラーにします(上限を超えた suite は待っても見えないため)。run が 1 件でも見つかれば、結論に関わらず終了コード 0 です(`runs` は成否を判定しません)。
- `--json` は作成時刻の新しい順(同時刻は run id の大きい順)の配列で、並びは決定的です。要素は `run_id` / `run_number` / `run_attempt` / `workflow` / `workflow_path`(動的 workflow は null)/ `event` / `status` / `conclusion`(`gh` と同じ小文字表記: `completed`・`success` など。実行中の `conclusion` は null)/ `created_at` / `url` / `head_sha` / `branch`(ブランチ起点でなければ null)です。
- キャッシュは完了済みでも `--ttl` を超えて使い回さないため、`runs` は新しい run やブランチ先頭の移動を、`watch` / `status` は再実行後の結果を拾えます。ただしレート制限中は例外で、古いキャッシュで代替し得ます(「レート制限への振る舞い」の節を参照)。

### `gh run watch` の置き換え

| これまで | Vigilia |
| --- | --- |
| `gh run watch <run-id> --exit-status` | `vigilia watch <run-id>` |
| push 直後に最新 run を待つ | `vigilia watch HEAD`(SHA 起点。run が現れるまで `--wait-for-runs` 秒待つ) |
| `gh run view <run-id>` | `vigilia status <run-id>` |
| `gh api rate_limit` | `vigilia budget` |
| `gh run list --commit <sha> --json databaseId,...` | `vigilia runs <sha> --json` |
| release workflow の run が現れるまで `gh run list --branch <b> --commit <sha>` を polling | `vigilia runs <sha> --workflow release.yml --since <時刻> --wait 120 --json` |
| 特定の workflow だけ待つ | `vigilia watch <sha> --workflow release.yml` |

```powershell
git push; vigilia watch HEAD; if ($LASTEXITCODE -ne 0) { "CI NG: $LASTEXITCODE" }
```

### 終了コード

| コード | 意味 |
| ---: | --- |
| 0 | 成功(スキップ・中立は成功扱い) |
| 1 | 失敗(failure / timed_out / startup_failure / action_required) |
| 2 | 入力不正・認証・通信・API エラー |
| 3 | 取消(cancelled / stale) |
| 4 | `watch` が `--timeout` で打ち切られた |
| 5 | レート制限で結論を得られなかった |
| 6 | `status` で対象がまだ実行中・待機中 |
| 7 | 対象(リポジトリ・コミット・ブランチ・run)が見つからない / (条件に合う)run がまだ無い。`runs` では条件に合う run が見つからない |

## レート制限への振る舞い

- リクエストは常に直列で、`watch` の取得間隔は 5 秒以上に制限しています。
- `Retry-After` / `x-ratelimit-reset` を尊重します。`watch` は制限に当たっても落ちず、示された時間だけ待って再開します
  (`--timeout` を超えそうなら終了コード 5 で返します)。`status` は、キャッシュが無ければ終了コード 5、古いキャッシュがあれば警告付きでその断面を表示したうえで終了コード 5 で返します(古い断面を結論としては扱いません)。
- GraphQL の `RATE_LIMIT`(旧表記 `RATE_LIMITED` も同様。HTTP 200 で返る場合)も同様に扱います。制限中に古いキャッシュがあれば警告付きで表示します。`watch` は表示しますが、それを根拠に結論(完了・未検出)を確定せず、待って再取得します。
- 通信エラーと 5xx は最大 3 回試行します(再試行 2 回、待ちは 1 秒・2 秒)。

## キャッシュ

- 場所: Windows `%LOCALAPPDATA%\Vigilia\cache` / macOS `~/Library/Caches/Vigilia/cache` / Linux `$XDG_CACHE_HOME/Vigilia/cache`
  (環境変数 `VIGILIA_CACHE_DIR` で変更可能)。
- キー(repo + 対象)ごとに 1 ファイル。一時ファイルへ書いてから rename するため、読み手が壊れた内容を見ることはありません。
- スナップショットは完了済みでも `--ttl`(既定 15 秒)以内だけ再利用します(再実行で結果が変わるため)。ただしレート制限中は例外で、古いキャッシュで代替し得ます(「レート制限への振る舞い」の節を参照)。`--no-cache` で無効化できます。
- キャッシュは最適化であり正本ではありません。壊れた・古い・読めないファイルは無視して再取得します。

## 既知の制約

- run id → run の解決には、GitHub が非推奨としているレガシー形式のグローバル ID(`011:WorkflowRun<id>`)を使います。
  REST を使わずに run id から引ける経路がこれしかないためです。将来廃止された場合は run id 指定だけが API エラーになります
  (SHA 指定は影響を受けません)。
- 1 コミットあたり check suite は 50 件、1 suite あたり check run は 30 件(run id 指定は 50 件)まで取得します。
  suite が上限を超えているときは、取りこぼした suite が結論を覆し得るため、成功・取消・run なしは断定せずエラー(終了コード 2)にします。
  失敗は、絞り込みなしなら確定できますが、`--workflow` / `--event` で絞り込んだ後は(新しい run を取りこぼした可能性があるため)エラーです。
  `runs` は一致する run があれば 0 を返し、取りこぼしがあれば stderr に注記します(一致が 0 件なら 7 ではなくエラーの終了コード 2)。job の超過は「一部を省略」と表示します。
- check run を持たない非 Actions アプリの check suite は、完了しないことがあるため監視対象から除外します。

## 開発

```sh
cargo build --locked
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

## ライセンス

[MIT](LICENSE)
