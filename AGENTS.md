# AGENTS.md

Vigilia は、GitHub Actions の状態を GraphQL で取得し、ファイルキャッシュで複数セッションに共有する Rust 製 CLI（単一クレート `vigilia`、edition 2024）。
目的・構成・設計判断は [DESIGN.md](DESIGN.md)、利用者向けの使い方は [README.md](README.md)（英語）と [README.ja.md](README.ja.md)（日本語）を参照する。README は両言語を同じ内容に揃えて更新する（英語の詳細は `docs/readme/` に分割、日本語は `README.ja.md` の1ファイル）。変更履歴は [CHANGELOG.md](CHANGELOG.md)（日本語）に版ごとにまとめる。

## 必須コマンド

CI（`.github/workflows/ci.yml`、windows / macos / ubuntu の3環境）と同じ検査を、変更後に通す。

```powershell
cargo build --locked
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

- `Cargo.lock` はコミット対象。CI は `--locked` なので、依存を変えたら lock も更新する。版は `Cargo.toml` の `version` が正本で、上げるときは `Cargo.lock` と CHANGELOG.md も揃える。
- `publish = false`（crates.io には公開しない）。配布は Cloudflare R2 で行う（`[profile.release]` は lto・strip で単一バイナリを小さく保つ）。

## リリースと配信

配布は Cloudflare R2 だけで、GitHub Release は使わない。配布物のビルドはキャッシュを使わない（rust-cache は `ci.yml` だけで、保存は main のみ）。
`release.yml`・`install.ps1` / `install.sh`・`scripts/` の変更やリリースの前に [docs/agents/release-process.md](docs/agents/release-process.md) を読む。

## 構成（`src/`）

モジュールごとの責務の表は、`src/` を変える前に [docs/agents/src.md](docs/agents/src.md) を読む。

## 守るべき制約

- **REST の枠を消費しない。** CI 状態の取得は GraphQL（`POST /graphql`）のみ。使ってよい REST は枠を消費しない `GET /rate_limit` だけ。REST 経由の取得を足さない。
- **リクエストは直列。** 並列化しない。`watch` と `runs --wait` の `--interval` は 5 秒未満を拒否する（`commands.rs` の `MIN_INTERVAL_SECS`）。
- **トークンを出さない。** `GH_TOKEN` / `GITHUB_TOKEN` → `gh auth token` の順で取得し、ログ・エラー文・出力・キャッシュへ書かない。
- **終了コードの表は3か所を揃える。** `error.rs` の `exit`、`main.rs` の `watch` の doc コメント、README の表。コードを変えたら全て更新する。
- **結論の規則は `model.rs` に集約する。** `watch` と `status` は同じ `conclusive_overall()` を使う。suite が切り捨てられた状態では、成功・取消・run なし（絞り込み後は失敗も）を断定しない（エラーにする。`conclusive_overall(filtered)`）。`--workflow` / `--event` の絞り込み（複数一致の規則を含む）も `Snapshot::filtered()` に置き、取得後に適用する（キャッシュのキーに条件を入れない）。
- **キャッシュは最適化であり正本ではない。** 読めない・壊れた・版違いは「なし」として扱い、書き込みは一時ファイル → rename。`cache.rs` の `ENTRY_VERSION` は `Snapshot` の形式を非互換に変えたときに上げる。
- **ユーザー向け文言・コードコメントは日本語。** 既存の書き方に合わせる。

## テスト

- 単体テストは各ソース内の `#[cfg(test)]`。テスト名は日本語。ネットワークと `gh` に依存しない（GraphQL 応答は JSON フィクスチャで検証する）。
- キャッシュのテストは `target/test-tmp/` 配下に作る。C ドライブの TEMP は使わない。
- 実 API の動作確認は `vigilia budget` や `vigilia status <SHA> --repo OWNER/REPO` で行い、`VIGILIA_CACHE_DIR` で隔離できる。
