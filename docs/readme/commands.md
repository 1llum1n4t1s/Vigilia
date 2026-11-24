# Commands

Back to [README](../../README.md).

```text
vigilia watch  [<run-id | sha | HEAD>] [--repo OWNER/REPO] [--workflow <name|file>] [--event <event>] [--interval 20] [--timeout 3600] [--wait-for-runs 120] [--ttl 15] [--no-cache]
vigilia status [<run-id | sha | HEAD>] [--repo OWNER/REPO] [--workflow <name|file>] [--event <event>] [--json] [--ttl 15] [--no-cache]
vigilia runs   [<sha | HEAD | branch>] [--repo OWNER/REPO] [--workflow <name|file>] [--event <event>] [--since <RFC3339>] [--wait <secs>] [--interval 10] [--json] [--ttl 15] [--no-cache]
vigilia budget [--json]
```

- The target is a **run id** (9 or more digits) or a **commit SHA** (7 to 40 hex digits, abbreviations allowed). Omitting it or passing `HEAD` uses the current `HEAD`.
- Without `--repo`, the repository is inferred from the current git remote (`origin` first, github.com only; `ssh.github.com:443` SSH remotes count as github.com, GitHub Enterprise hosts are not supported). It is not needed for a run id.
- With a SHA, Vigilia watches every check suite (workflow) of that commit and concludes once all have finished. When several suites disagree, the result is failure > cancelled > success.
- `budget` prints the remaining REST, GraphQL and search quota and the reset times, without consuming any.

## Filters and listing

- `--workflow` / `--event` make `watch` and `status` conclude from the matching runs of a SHA target only. Matching rules, several runs and ambiguity: [Workflow filters](workflow.md).
- `runs` lists the workflow runs of a commit or branch tip and can wait for one to appear (`--wait`), without judging success. Filters, waiting and `--json` fields: [`runs` reference](api-reference.md).

## Replacing `gh run watch`

| Before | Vigilia |
| --- | --- |
| `gh run watch <run-id> --exit-status` | `vigilia watch <run-id>` |
| Wait for the latest run right after a push | `vigilia watch HEAD` (SHA based; waits up to `--wait-for-runs` seconds for a run to appear) |
| `gh run view <run-id>` | `vigilia status <run-id>` |
| `gh api rate_limit` | `vigilia budget` |
| `gh run list --commit <sha> --json databaseId,...` | `vigilia runs <sha> --json` |
| Poll `gh run list --branch <b> --commit <sha>` until the release workflow's run appears | `vigilia runs <sha> --workflow release.yml --since <time> --wait 120 --json` |
| Wait only for one workflow | `vigilia watch <sha> --workflow release.yml` |

```powershell
git push; vigilia watch HEAD; if ($LASTEXITCODE -ne 0) { "CI NG: $LASTEXITCODE" }
```

## Exit codes

See [Exit codes](../../README.md#exit-codes).
