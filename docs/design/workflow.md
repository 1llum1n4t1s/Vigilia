# データフロー

[DESIGN.md](../../DESIGN.md) の詳細。

1. `target.rs` が引数を `Target::Run(id)` か `Target::Sha(sha)` に分類する(`runs` だけはそれ以外の文字列を `Target::Branch` として受ける)。数字のみ9桁以上は run id、16進7〜40桁は SHA。`HEAD`・省略は `git rev-parse HEAD`。SHA のときの repo は `--repo`、なければ git remote（`origin` 優先、github.com のみ。ポート443のSSH用 `ssh.github.com` は github.com として扱う）から推定する。run id には repo を要求しない。
2. `Resolver::get()` がキャッシュキー（`run__<id>` または `<owner>__<repo>__sha__<sha>`）で `cache.get_or_lock()` を呼ぶ。
   - 新鮮なエントリがあれば返す（`Source::Cache`）。
   - なければロックを取り、ダブルチェックしたうえで GraphQL を1回実行し、結果を書き込む（`Source::Api`）。
   - 他セッションがロック保持中なら最大8秒、更新を待つ。それでも更新されなければ自分で取得する。
3. GraphQL 応答は `model.rs` で `Snapshot`（suite → job の正規化済みツリー）に変換される。キャッシュ・`--json` 出力は `Snapshot` をそのまま使う。
4. `Snapshot::overall()` が全 suite を畳み込んで結論を出し、`commands.rs` が終了コードに変換する。
5. `watch` は `--interval` ごとに 1〜4 を繰り返し、状態が変わった job だけを出力し、結論が確定したら終了する。

クエリは2種類。SHA 起点は `repository.object(expression)` から `checkSuites(first:50)` と各 `checkRuns(first:30)`、run id 起点は `node(id)` から `checkSuite` と `checkRuns(first:50)`。どちらも各 suite から `branch{name}`、`workflowRun{databaseId runNumber runAttempt createdAt event url workflow{name} file{path}}` を取る（`runs` と `--workflow` に必要な分。追加は connection ではなくスカラー / 単一オブジェクトなので cost は 1 のまま）。

## `runs` と `--workflow` / `--event`

追加クエリは作らず、既存の SHA 起点クエリ・キャッシュ・待機の仕組みをそのまま使う。
- **ブランチ指定**は `object(expression: "refs/heads/<名前>")` で先頭コミットへ解決する（同名タグとの衝突を避けるため完全な ref 名にする）。`Target::Branch` はこのクエリ用で、`watch` / `status` は受け付けない。
- **絞り込みは取得の後**に `Snapshot` へ適用する（`model.rs` の `RunFilter` / `Snapshot::filtered()` / `Snapshot::runs()`）。キャッシュのキーに条件を含めないので、条件違いのセッションが同じ取得結果を安全に共有できる。ブランチのキーだけは、ファイル名で `/` と `_` が潰れて衝突しないようハッシュを添える。
- **キャッシュの再利用は完了済みでも `--ttl` 以内だけ。** 再実行で run id のまま結果が変わり、絞る前の全体が完了済みでも待っている run が未出現なだけの場合もあるため、結論の確定を再利用の根拠にしない。絞り込み条件は `Resolver` が取得後に適用する（キャッシュのキーには入れない）。
- **複数一致の規則**（`Snapshot::filtered`）: 指定が複数の別 workflow に一致したら使用エラー（終了コード 2）。同じ (workflow, event) の run は最新の 1 件だけを結論に使う（再実行・重複 push で古い失敗が新しい成功を隠さない）。event が違う run は別物として全て含める。`runs` は一致した run を全て列挙する。
- **workflow の一致**は、表示名・ファイルパス・ファイル名のいずれかと完全一致（大文字小文字を区別）。動的 workflow（Dependabot Updates など）はパスを持たないので表示名だけで指定できる。
- **`runs --wait`** は `watch` と同じ最小間隔（5 秒）・レート制限待機・古いキャッシュ時の待機を使い、現れなければ終了コード 7。期限までの残りが 5 秒未満になったら、端数だけ待って取得し直さず期限処理へ進む（`watch` の `--timeout` も同じ。5 秒未満の間隔で API を叩かない）。`runs` は結論を判定せず、run が 1 件でもあれば 0。
