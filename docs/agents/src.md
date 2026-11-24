# `src/` の構成

[AGENTS.md](../../AGENTS.md) の詳細。

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
