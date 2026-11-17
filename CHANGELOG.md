# Changelog

## 1.0.0 - 2026-11-18

### 追加

- `vigilia watch`: コミット SHA(`HEAD` 可)または run id を指定し、GitHub Actions の全 check suite が完了するまで監視して結論を終了コードで返す。状態が変わった job だけを表示する。
- `vigilia status`: 状態を 1 回だけ取得して表示する。`--json` で機械可読な出力にできる。
- `vigilia budget`: REST / GraphQL / search の残り枠とリセット時刻を、枠を消費せずに表示する。
- CI 状態の取得を GraphQL のみで行い、REST の API 枠(5,000 回/時)を消費しない。REST の枠が尽きていても動作する。
- 取得結果のファイルキャッシュを複数セッションで共有し、同じ対象への問い合わせを集約する(`--ttl`、`--no-cache`、`VIGILIA_CACHE_DIR`)。
- レート制限に当たっても `watch` は落ちず、示された時間だけ待って再開する。制限中は古いキャッシュを警告付きで返す。
- 結論を終了コードで区別する(成功 0 / 失敗 1 / エラー 2 / 取消 3 / タイムアウト 4 / レート制限 5 / 実行中 6 / 未検出 7)。
- check suite の取得が上限で切り捨てられ、成功と断定できない場合は成功とせずエラーにする。
- `--repo` 省略時にカレントの git remote から対象リポジトリを推定する。
- 認証は `GH_TOKEN` / `GITHUB_TOKEN`、なければ `gh auth token` から取得する。
- Windows x64、macOS Apple Silicon、Linux で動作する。
