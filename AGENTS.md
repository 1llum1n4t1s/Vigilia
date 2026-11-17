# AGENTS.md

Vigilia は、GitHub Actions の状態を GraphQL で取得し、ファイルキャッシュで複数セッションに共有する Rust 製 CLI（単一クレート `vigilia`、edition 2024）。
目的・構成・設計判断は [DESIGN.md](DESIGN.md)、利用者向けの使い方は [README.md](README.md)（英語）と [README.ja.md](README.ja.md)（日本語）を参照する。README は両言語を同じ内容に揃えて更新する。変更履歴は [CHANGELOG.md](CHANGELOG.md)（日本語）に版ごとにまとめる。

## 必須コマンド

CI（`.github/workflows/ci.yml`、windows / macos / ubuntu の3環境）と同じ検査を、変更後に通す。

```powershell
cargo build --locked
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

- `Cargo.lock` はコミット対象。CI は `--locked` なので、依存を変えたら lock も更新する。版は `Cargo.toml` の `version` が正本で、上げるときは `Cargo.lock` と CHANGELOG.md も揃える。
- `publish = false`（crates.io には公開しない）。配布は下の「リリースと配信」のとおり Cloudflare R2 で行う（`[profile.release]` は lto・strip で単一バイナリを小さく保つ）。

## リリースと配信

配布は Cloudflare R2（バケット `vigilia-updates`、公開 URL https://vigilia.kagayoi.com）だけで、タグと GitHub Release は使わない。

- 手順: `release/<版>` ブランチを push する（ブランチ名と `Cargo.toml` の版は一致させる。版を上げるのは `/vava`）。`.github/workflows/release.yml` が guard → build（4 ターゲット）→ sign-windows → publish の順で動き、R2 へ配信する。同じ release ブランチでの再実行だけ workflow_dispatch を使う（`skip_upload` で配信なしの通し確認もできる）。
- 同時実行: `release.yml` の concurrency は repo 全体で `group: release`（`cancel-in-progress: false`）。別の release ブランチが並走しても `latest.json` を取り合って後退させない。
- ターゲット: Windows x64（`x86_64-pc-windows-msvc`）、macOS Apple Silicon（`aarch64-apple-darwin`）、Linux x64 / arm64（`x86_64-unknown-linux-gnu` / `aarch64-unknown-linux-gnu`）。macOS Intel・Windows arm64 は対象外。ターゲット表は `release.yml` の matrix・`scripts/Publish-R2.ps1`・`install.ps1` / `install.sh`・`tests/Test-Install.ps1` で揃える。
- `install.ps1` は例外として、コメントも文字列も ASCII（英語）だけで書く。保存して Windows PowerShell 5.1 で実行されると、BOM なし UTF-8 を ANSI(CP932) として読み、日本語が改行を巻き込んで構文が壊れるため（`tests/Test-Install.ps1` が ASCII であることと 5.1 での実行を確かめる）。
- Linux の glibc: build ジョブが `objdump -T` で要求する glibc のシンボル版をログに残すだけで、下限は保証しない（配布物は ubuntu-24.04 でビルド）。`install.sh` は配置前に展開した `vigilia --version` を実行し、動かなければ置き換えない。
- テスト: `ci.yml` が全 OS で `tests/Test-Install.ps1`（Windows では `install.ps1` と Windows PowerShell 5.1 も）を実行し、`release.yml` の Linux x64 build が `cargo test --locked` を通す。
- 署名: Windows の exe だけ、CI 内で Sigilune（Certum のクラウド鍵）が署名し、署名者 CN を検証する。macOS は公証なしで、リンカーの ad-hoc 署名を `codesign --verify` で確かめるまで。
- R2 の置き場: `<版>/vigilia-<版>-<ターゲット>.zip`（Windows）・`.tar.gz`（macOS / Linux）と `.sha256`（版付き・不変・`public, max-age=31536000, immutable`）、最後に `latest.json`（固定名・`no-cache, must-revalidate`、形式は `scripts/Publish-R2.ps1` の冒頭を参照）。版付きは新規作成のみで、同内容なら成功・別内容なら失敗する。公開 URL から取り直して照合してから `latest.json` を置く。公開中の `latest.json` がより新しい版なら更新しない。
- GitHub の設定: Environment `release`（許可 ref は `release/*` だけ、承認者なし）に Secrets `SIGILUNE_EMAIL` / `SIGILUNE_TOTP_URI`（署名）と `CLOUDFLARE_API_TOKEN`（配信。R2 の書込みとキャッシュ purge 権限）を置く。Secrets は sign-windows の署名 step と publish の配信 step にだけ渡し、build・梱包・検証 step には渡さない。Cloudflare のアカウント ID は秘密ではなく `Publish-R2.ps1` の既定値に持つ。
- Cloudflare 側: R2 バケット `vigilia-updates` を作り、カスタムドメイン `vigilia.kagayoi.com`（zone `kagayoi.com`）を割り当てる。
- `install.ps1` / `install.sh` / `scripts/Publish-R2.ps1` / `scripts/R2*` を変えたら `pwsh -NoProfile -File tests/Test-Install.ps1` を通す（R2 にも Certum にも接続しない）。`R2Transfer.Core.cs` と `R2Distribution.Common.ps1` は Sigilune・Edictora・Skilvella と共通の同梱コピーなので、片方だけ直さない。
- `release.yml` の配信 step で `$LASTEXITCODE` を見ない。`Publish-R2.ps1` は失敗を例外で知らせ、成功後もスクリプト内部の終了コードが残るため、見ると誤って失敗扱いになる。
- 配布物のビルドはキャッシュを使わない（`ci.yml` と違い rust-cache を付けない）。再ビルドは同じ版でもバイト列が変わるため、配信済みの版を別のビルドで上書きしようとすると失敗する。配信だけやり直すときは publish ジョブだけを再実行する。

## 構成（`src/`）

| ファイル | 責務 |
| --- | --- |
| `main.rs` | clap の引数定義と `main`。エラーを終了コードへ変換する |
| `commands.rs` | `watch` / `status` / `runs` / `budget` の実装 |
| `service.rs` | `Resolver`。キャッシュ → GraphQL の取得を1か所に集約(`Freshness` で完了済みの再利用方針を切り替える) |
| `cache.rs` | ファイルキャッシュ・ロック |
| `github.rs` | GraphQL / `GET /rate_limit` クライアント、トークン取得、HTTP エラー分類 |
| `model.rs` | `Snapshot` と結論（`Overall`）の判定、GraphQL 応答の正規化、workflow / event の絞り込み(`RunFilter`)と run 一覧(`RunInfo`) |
| `target.rs` | 対象（run id / SHA / HEAD / ブランチ(`runs` のみ)）と repo の解釈・推定 |
| `render.rs` | 表示用の整形 |
| `error.rs` | `VigiliaError` と終了コード |

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
