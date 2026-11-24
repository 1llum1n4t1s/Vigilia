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

引数 → `target.rs` で対象を分類 → `Resolver::get()` がキャッシュ、なければ GraphQL 1回 → `Snapshot` に正規化 → 結論を判定 → 終了コード。クエリと、`runs`・`--workflow` / `--event` の絞り込みの詳細は [docs/design/workflow.md](docs/design/workflow.md)。

## 不変条件

- REST の枠を消費しない（`POST /graphql` と `GET /rate_limit` だけ）。リクエストは直列で、間隔は 5 秒以上。
- トークンはメモリ内だけで保持し、どこへも書かない。
- 結論の規則は `model.rs` の1か所。切り捨てた状態で結論を断定しない。
- キャッシュは最適化であり正本ではない。
- 終了コードは機械可読な契約（定義は `error.rs`）。

判定規則と例外の詳細は [docs/design/conventions.md](docs/design/conventions.md)。

## 採用した設計判断

常駐デーモンなしのファイル協調、TTL、レート制限時の待機、レガシー形式のグローバル ID、suite の絞り込みなど。理由とトレードオフは [docs/design/architecture.md](docs/design/architecture.md)。

## 配布

ビルド済みバイナリを Cloudflare R2 だけから配り、版付きは不変、`latest.json` を最後に更新する。設計は [docs/design/release-process.md](docs/design/release-process.md)。

## 既知の制約

- 1コミットあたり check suite 50件、1 suite あたり check run 30件（run id 起点は50件）を超える分は取得しない。
- 配布するのは Windows x64（主）、macOS Apple Silicon、Linux x64 / arm64（glibc）。Intel Mac と Windows arm64 は対象外。Linux のキャッシュ先は `$XDG_CACHE_HOME`。
- github.com のみ（`ssh.github.com:443` 経由の SSH は github.com として扱う。GitHub Enterprise のホストは扱わない）。
