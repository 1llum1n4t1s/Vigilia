# Quick start

Back to [README](../../README.md).

```sh
git push
vigilia watch HEAD      # wait until every workflow for HEAD finishes
vigilia status HEAD     # print the current state once
vigilia budget          # show remaining REST / GraphQL / search quota
```

Example:

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

Command output is in Japanese. `watch` prints only the jobs whose state changed; `status` prints every job once, and `--json` makes it machine readable (`overall`, `source`, `age_secs`, `filter`, `snapshot`). When only a stale cache is available under rate limiting, the text heading reads "結論: 未確定(レート制限中の古い断面: ...)" instead of a conclusion, and `--json` sets `overall` to null and puts the value computed from the stale snapshot in an extra field `stale_overall` (`source` is `stale-cache`; the exit code is 5).
