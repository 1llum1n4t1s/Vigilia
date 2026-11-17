**English** | [日本語](README.ja.md)

# Vigilia

Vigilia is a small CLI that watches GitHub Actions **without spending your GitHub REST API rate limit**. It reads CI state through the separate **GraphQL** quota and **shares the result between sessions through a file cache**. Published by Kagayoi.

## The problem

When several AI agent sessions (or scripts) run under the same GitHub account and poll with `gh run watch` or `gh run view`, they all draw from **one REST quota per authenticated user (5,000 requests per hour)**. Once it is exhausted, every session fails with `HTTP 403: API rate limit exceeded`, and waiting does not make the total faster. Vigilia addresses this in two ways:

1. **It uses GraphQL only.** The GraphQL quota (5,000 points per hour) is counted separately from REST and is normally almost unused. `checkSuites` / `workflowRun` / `checkRuns` are combined into one query that costs a few points per request. Vigilia keeps working when the REST quota is 0: it only calls `POST /graphql` and `GET /rate_limit`, which does not consume quota.
2. **It shares results.** Fetched snapshots are stored in a local cache. While a snapshot is fresh (TTL 15 seconds by default), other sessions watching the same target get the cached copy instead of querying again, and concurrent fetches are collapsed into one with a lightweight lock. There is no daemon; sessions coordinate through files only.

Internals and design decisions are in [DESIGN.md](DESIGN.md); contributor conventions are in [AGENTS.md](AGENTS.md).

## Installation

No prebuilt binaries are published. Install from source with Rust (a stable toolchain that supports edition 2024):

```sh
cargo install --git https://github.com/1llum1n4t1s/Vigilia
```

or clone the repository and build it:

```sh
cargo build --release   # target/release/vigilia (vigilia.exe on Windows)
```

CI builds, tests and lints on Windows x64, Apple Silicon macOS and Linux. Intel Macs are not supported.

Authentication borrows your existing GitHub CLI login (`gh auth token`), so `gh` must be installed and signed in. If `GH_TOKEN` or `GITHUB_TOKEN` is set, it takes precedence. The token is never written to logs, output or the cache.

## Quick start

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

Command output is in Japanese. `watch` prints only the jobs whose state changed; `status` prints every job once, and `--json` makes it machine readable.

## Commands

```text
vigilia watch  [<run-id | sha | HEAD>] [--repo OWNER/REPO] [--interval 20] [--timeout 3600] [--wait-for-runs 120] [--ttl 15] [--no-cache]
vigilia status [<run-id | sha | HEAD>] [--repo OWNER/REPO] [--json] [--ttl 15] [--no-cache]
vigilia budget [--json]
```

- The target is a **run id** (9 or more digits) or a **commit SHA** (7 to 40 hex digits, abbreviations allowed). Omitting it or passing `HEAD` uses the current `HEAD`.
- Without `--repo`, the repository is inferred from the current git remote (`origin` first, github.com only). It is not needed for a run id.
- With a SHA, Vigilia watches every check suite (workflow) of that commit and concludes once all have finished. When several suites disagree, the result is failure > cancelled > success.
- `budget` prints the remaining REST, GraphQL and search quota and the reset times, without consuming any.

### Replacing `gh run watch`

| Before | Vigilia |
| --- | --- |
| `gh run watch <run-id> --exit-status` | `vigilia watch <run-id>` |
| Wait for the latest run right after a push | `vigilia watch HEAD` (SHA based; waits up to `--wait-for-runs` seconds for a run to appear) |
| `gh run view <run-id>` | `vigilia status <run-id>` |
| `gh api rate_limit` | `vigilia budget` |

```powershell
git push; vigilia watch HEAD; if ($LASTEXITCODE -ne 0) { "CI NG: $LASTEXITCODE" }
```

### Exit codes

| Code | Meaning |
| ---: | --- |
| 0 | Success (skipped and neutral count as success) |
| 1 | Failure (failure / timed_out / startup_failure / action_required) |
| 2 | Invalid input, authentication, network or API error |
| 3 | Cancelled (cancelled / stale) |
| 4 | `watch` stopped by `--timeout` |
| 5 | Could not reach a conclusion because of rate limiting |
| 6 | `status`: the target is still running or queued |
| 7 | The target (repository, commit or run) was not found, or no run exists yet |

## Rate limits

- Requests are always sequential, and the `watch` interval is limited to 5 seconds or more.
- `Retry-After` and `x-ratelimit-reset` are honored. `watch` does not abort when it is rate limited: it waits for the indicated time and resumes (it returns exit code 5 if that would exceed `--timeout`). `status` returns exit code 5.
- A GraphQL `RATE_LIMITED` error (which arrives with HTTP 200) is handled the same way. While limited, a stale cached snapshot is returned with a warning if one exists.
- Network errors and 5xx responses are retried up to 3 times with exponential backoff.

## Caching

- Location: Windows `%LOCALAPPDATA%\Vigilia\cache`, macOS `~/Library/Caches/Vigilia/cache`, Linux `$XDG_CACHE_HOME/Vigilia/cache`. Override it with the `VIGILIA_CACHE_DIR` environment variable.
- One file per key (repository plus target). Files are written to a temporary file and renamed, so a reader never sees a half-written entry.
- A finished snapshot (conclusion fixed) is reused for up to 5 minutes. `--no-cache` disables the cache.
- The cache is an optimization, not a source of truth. Corrupt, outdated or unreadable files are ignored and refetched.

## Limitations

- Resolving a run id to a run uses the legacy global ID format that GitHub has deprecated (`011:WorkflowRun<id>`), because it is the only way to look up a run by id without REST. If it is removed in the future, only run id targets will fail with an API error; SHA targets are unaffected.
- At most 50 check suites per commit and 30 check runs per suite (50 for a run id target) are fetched. If suites exceed the limit and everything seen looks successful, Vigilia reports an error (exit code 2) instead of claiming success; exceeding the job limit is shown as partially omitted.
- Check suites of non-Actions apps that have no check runs may never complete, so they are excluded from watching.

## Development

```sh
cargo build --locked
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

## License

[MIT](LICENSE)
