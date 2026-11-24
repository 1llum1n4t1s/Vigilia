# 配布の設計

[DESIGN.md](../../DESIGN.md) の詳細。

利用者は CI も Rust も不要で使えるよう、ビルド済みバイナリを Cloudflare R2 から配る（運用手順は [リリースと配信](../agents/release-process.md)）。

- **R2 だけを入口にする。** GitHub Release は使わない。main のコミットに付けた軽量タグ `v<版>` の push で `release.yml` が動く（guard が、タグであること・`v<版>` と `Cargo.toml` の版の一致・軽量タグ・main の祖先であることを確かめる）。本人の他製品（Sigilune・Skilvella など）と同じ方式で、配信の転送実装（`scripts/R2Transfer.Core.cs`）も共通の同梱コピーを使う。
- **版付きは不変、最後に `latest.json`。** 版付きのアーカイブと `.sha256` を新規作成だけで置き、公開 URL から取り直して照合してからフィードを進める。途中で失敗しても、利用者が見る `latest.json` は古い完全な版のまま残る。
- **`latest.json` は複数ターゲットを表す。** 導入スクリプトは版とターゲットからアーカイブのパスを自分で組み立て、フィードの値は照合にだけ使う（任意のパスへ誘導されない）。
- **導入スクリプトは検証してから置く。** どちらも https のみ・リダイレクトを辿らない・SHA-256 照合。Windows は Authenticode 署名者の CN も照合する。macOS は公証なしの ad-hoc 署名で、導入時は署名の整合だけ確かめる。
- **Secrets を持つ job を最小にする。** ビルドと梱包は Secrets なしの job で行い、署名と配信だけを Environment `release` の job に置く。
