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

`watch` は状態が変わった job だけを出力します。`status` は全 job を 1 回表示し、`--json` で機械可読にできます。

## コマンド

```text
vigilia watch  [<run-id | sha | HEAD>] [--repo OWNER/REPO] [--interval 20] [--timeout 3600] [--wait-for-runs 120] [--ttl 15] [--no-cache]
vigilia status [<run-id | sha | HEAD>] [--repo OWNER/REPO] [--json] [--ttl 15] [--no-cache]
vigilia budget [--json]
```

- 対象は **run id**(数字 9 桁以上)または **コミット SHA**(16 進 7〜40 桁、短縮可)。省略または `HEAD` でカレントの `HEAD`。
- `--repo` を省略するとカレントの git remote(`origin` 優先、github.com のみ)から推定します。run id のときは不要です。
- SHA を指定すると、そのコミットの全 check suite(ワークフロー)をまとめて監視し、すべて完了した時点で結論を出します。
  複数 suite の結論は 失敗 > 取消 > 成功 の優先順です。
- `budget` は REST / GraphQL / search の残り枠とリセット時刻を、枠を消費せずに表示します。

### `gh run watch` の置き換え

| これまで | Vigilia |
| --- | --- |
| `gh run watch <run-id> --exit-status` | `vigilia watch <run-id>` |
| push 直後に最新 run を待つ | `vigilia watch HEAD`(SHA 起点。run が現れるまで `--wait-for-runs` 秒待つ) |
| `gh run view <run-id>` | `vigilia status <run-id>` |
| `gh api rate_limit` | `vigilia budget` |

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
| 7 | 対象(リポジトリ・コミット・run)が見つからない / run がまだ無い |

## レート制限への振る舞い

- リクエストは常に直列で、`watch` の取得間隔は 5 秒以上に制限しています。
- `Retry-After` / `x-ratelimit-reset` を尊重します。`watch` は制限に当たっても落ちず、示された時間だけ待って再開します
  (`--timeout` を超えそうなら終了コード 5 で返します)。`status` は終了コード 5 で返します。
- GraphQL の `RATE_LIMITED`(HTTP 200 で返る場合)も同様に扱います。制限中に古いキャッシュがあれば警告付きでそれを返します。
- 通信エラーと 5xx は最大 3 回、指数バックオフで再試行します。

## キャッシュ

- 場所: Windows `%LOCALAPPDATA%\Vigilia\cache` / macOS `~/Library/Caches/Vigilia/cache` / Linux `$XDG_CACHE_HOME/Vigilia/cache`
  (環境変数 `VIGILIA_CACHE_DIR` で変更可能)。
- キー(repo + 対象)ごとに 1 ファイル。一時ファイルへ書いてから rename するため、読み手が壊れた内容を見ることはありません。
- 完了済み(結論が確定)のスナップショットは最大 5 分間再利用します。`--no-cache` で無効化できます。
- キャッシュは最適化であり正本ではありません。壊れた・古い・読めないファイルは無視して再取得します。

## 既知の制約

- run id → run の解決には、GitHub が非推奨としているレガシー形式のグローバル ID(`011:WorkflowRun<id>`)を使います。
  REST を使わずに run id から引ける経路がこれしかないためです。将来廃止された場合は run id 指定だけが API エラーになります
  (SHA 指定は影響を受けません)。
- 1 コミットあたり check suite は 50 件、1 suite あたり check run は 30 件(run id 指定は 50 件)まで取得します。
  suite が上限を超えて成功に見える場合は成功と断定せずエラー(終了コード 2)にし、job の超過は「一部を省略」と表示します。
- check run を持たない非 Actions アプリの check suite は、完了しないことがあるため監視対象から除外します。

## 開発

```sh
cargo build --locked
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

## ライセンス

[MIT](LICENSE)
