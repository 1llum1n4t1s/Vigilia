**English** | [日本語](README.ja.md)

# Vigilia

Vigilia is a small CLI that watches GitHub Actions **without spending your GitHub REST API rate limit**. It reads CI state through the separate **GraphQL** quota and **shares the result between sessions through a file cache**. Published by Kagayoi.

## The problem

Parallel sessions polling with `gh run watch` share one REST quota (5,000 requests per hour) and exhaust it. See [The problem](docs/readme/the-problem.md) for how Vigilia avoids this.

## Installation

Windows x64 (PowerShell):

```powershell
irm https://raw.githubusercontent.com/1llum1n4t1s/Vigilia/main/install.ps1 | iex
```

macOS (Apple Silicon) and Linux (x86_64 / aarch64, glibc):

```sh
curl -fsSL https://raw.githubusercontent.com/1llum1n4t1s/Vigilia/main/install.sh | sh
```

`gh` must be signed in, or set `GH_TOKEN` / `GITHUB_TOKEN`. Pinning a version, direct downloads and building from source: [Installation](docs/readme/installation.md).

## Quick start

```sh
vigilia watch HEAD      # wait until every workflow for HEAD finishes
vigilia status HEAD     # print the current state once
vigilia runs HEAD       # list the workflow runs
vigilia budget          # show remaining REST / GraphQL / search quota
```

Example output: [Quick start](docs/readme/quick-start.md). All options, workflow filters and `gh` replacements: [Commands](docs/readme/commands.md).

## Exit codes

| Code | Meaning |
| ---: | --- |
| 0 | Success (skipped and neutral count as success) |
| 1 | Failure (failure / timed_out / startup_failure / action_required) |
| 2 | Invalid input, authentication, network or API error |
| 3 | Cancelled (cancelled / stale) |
| 4 | `watch` stopped by `--timeout` |
| 5 | Could not reach a conclusion because of rate limiting |
| 6 | `status`: the target is still running or queued |
| 7 | The target (repository, commit, branch or run) was not found, or no (matching) run exists yet; `runs`: no matching run found |

## More

In a downloaded archive the relative links do not resolve; read the [online docs](https://github.com/1llum1n4t1s/Vigilia/tree/main/docs/readme) instead.

- [Rate limits](docs/readme/rate-limits.md): how waiting and retries work.
- [Caching](docs/readme/caching.md): location and reuse rules.
- [Limitations](docs/readme/limitations.md): fetch limits and excluded suites.
- [DESIGN.md](DESIGN.md): internals; [AGENTS.md](AGENTS.md): contributor conventions.

## Development

```sh
cargo build --locked
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

## License

[MIT](LICENSE)
