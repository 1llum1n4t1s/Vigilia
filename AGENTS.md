# AGENTS.md

Vigilia は、GitHub Actions の状態を GraphQL で取得し、ファイルキャッシュで複数セッションに共有する Rust 製 CLI（単一クレート `vigilia`、edition 2024）。
目的・構成・設計判断は [DESIGN.md](DESIGN.md)、利用者向けの使い方は [README.md](README.md) を参照する。

## 必須コマンド

CI（`.github/workflows/ci.yml`、windows / macos / ubuntu の3環境）と同じ検査を、変更後に通す。

```powershell
cargo build --locked
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

- `Cargo.lock` はコミット対象。CI は `--locked` なので、依存を変えたら lock も更新する。
- `publish = false`。リリース配信は未整備（`[profile.release]` は lto・strip で単一バイナリを小さく保つ）。

## 構成（`src/`）

| ファイル | 責務 |
| --- | --- |
| `main.rs` | clap の引数定義と `main`。エラーを終了コードへ変換する |
| `commands.rs` | `watch` / `status` / `budget` の実装 |
| `service.rs` | `Resolver`。キャッシュ → GraphQL の取得を1か所に集約 |
| `cache.rs` | ファイルキャッシュ・ロック |
| `github.rs` | GraphQL / `GET /rate_limit` クライアント、トークン取得、HTTP エラー分類 |
| `model.rs` | `Snapshot` と結論（`Overall`）の判定、GraphQL 応答の正規化 |
| `target.rs` | 対象（run id / SHA / HEAD）と repo の解釈・推定 |
| `render.rs` | 表示用の整形 |
| `error.rs` | `VigiliaError` と終了コード |

## 守るべき制約

- **REST の枠を消費しない。** CI 状態の取得は GraphQL（`POST /graphql`）のみ。使ってよい REST は枠を消費しない `GET /rate_limit` だけ。REST 経由の取得を足さない。
- **リクエストは直列。** 並列化しない。`watch` の `--interval` は 5 秒未満を拒否する（`commands.rs` の `MIN_INTERVAL_SECS`）。
- **トークンを出さない。** `GH_TOKEN` / `GITHUB_TOKEN` → `gh auth token` の順で取得し、ログ・エラー文・出力・キャッシュへ書かない。
- **終了コードの表は3か所を揃える。** `error.rs` の `exit`、`main.rs` の `watch` の doc コメント、README の表。コードを変えたら全て更新する。
- **結論の規則は `model.rs` に集約する。** `watch` と `status` は同じ `conclusive_overall()` を使う。suite が切り捨てられた状態で「成功」と断定しない（エラーにする）。
- **キャッシュは最適化であり正本ではない。** 読めない・壊れた・版違いは「なし」として扱い、書き込みは一時ファイル → rename。`cache.rs` の `ENTRY_VERSION` は `Snapshot` の形式を非互換に変えたときに上げる。
- **ユーザー向け文言・コードコメントは日本語。** 既存の書き方に合わせる。

## テスト

- 単体テストは各ソース内の `#[cfg(test)]`。テスト名は日本語。ネットワークと `gh` に依存しない（GraphQL 応答は JSON フィクスチャで検証する）。
- キャッシュのテストは `target/test-tmp/` 配下に作る。C ドライブの TEMP は使わない。
- 実 API の動作確認は `vigilia budget` や `vigilia status <SHA> --repo OWNER/REPO` で行い、`VIGILIA_CACHE_DIR` で隔離できる。
