# Changelog

## Unreleased

### 追加

- `vigilia runs <sha|HEAD|ブランチ>`: コミットの workflow run 一覧(run id・workflow 名とファイルパス・event・状態・作成時刻・URL・ブランチ)を GraphQL だけで表示する。`--workflow` / `--event` / `--since` で絞り込み、`--wait <秒>` で条件に合う run が現れるまで待つ(現れなければ終了コード 7)。`--json` は作成時刻の新しい順の配列。`gh run list`(REST を消費する)の代わりに使える。
- `vigilia watch` / `vigilia status` に `--workflow <表示名|ファイルパス|ファイル名>` と `--event` を追加。SHA 指定のとき、指定した workflow の run だけで結論を判定する(run が現れるまでは待機 / 未検出)。同じ workflow・event の複数 run は最新だけを使い、event が違う run は全て含める。複数の workflow に一致する指定は使用エラーにする。
- キャッシュの形式を更新した(旧版のキャッシュは自動的に再取得される)。

## 1.0.1 - 2026-11-18

### 追加

- Windows x64、macOS Apple Silicon、Linux x64 / arm64 向けのビルド済みバイナリを `https://vigilia.kagayoi.com` から配布する。Windows 版はコード署名済み。各ファイルに SHA-256 を添える。
- 導入スクリプト `install.ps1`(Windows)と `install.sh`(macOS / Linux)を追加。最新版または指定した版を取得し、SHA-256(Windows は署名者も)を照合してから配置する。実行中の `vigilia` があっても置き換えられる。

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
