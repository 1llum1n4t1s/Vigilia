# `runs` reference

Back to [Commands](commands.md).

`vigilia runs` lists the workflow runs of a commit through GraphQL only, replacing `gh run list --commit ... --json ...` (a REST call). The target is a SHA, `HEAD`, or a **branch name** (resolved to its tip commit with the same query, as `refs/heads/<name>`; a run id is not accepted).

```text
$ vigilia runs abeebf669a1fbb239d7bf537c88293d0b98177af --repo 1llum1n4t1s/Vigilia --workflow release.yml
1llum1n4t1s/Vigilia @ abeebf6  run 1 件
  44146220753  成功  Release [.github/workflows/release.yml]  push  2026-11-17T18:17:24Z  release/1.0.1  https://github.com/.../runs/44146220753
```

- Filters: `--workflow` (matching as in [Workflow filters](workflow.md); a value matching several workflows lists all of them), `--event`, and `--since <RFC 3339>` (runs created at or after that time; a run without a creation time is excluded).
- `--wait <secs>` waits up to that long for a matching run to appear (default 0: fetch once). It polls every `--interval` seconds (default 10, minimum 5), honors rate-limit waits and shares the cache. If no run appears, the exit code is 7 (and `[]` is printed with `--json`); if check suites were truncated (see [Limitations](limitations.md)) and nothing matches, "no match" cannot be asserted and it is an error (exit code 2) at once, without waiting (suites past the limit never become visible by waiting). With one or more runs found, the exit code is 0 whatever their conclusions are; `runs` does not judge success.
- `--json` prints an array sorted by creation time, newest first (then run id, descending), so it is deterministic. Each element has `run_id`, `run_number`, `run_attempt`, `workflow`, `workflow_path` (null for dynamic workflows), `event`, `status` and `conclusion` (lower case like `gh`: `completed`, `success`, ...; `conclusion` is null while running), `created_at`, `url`, `head_sha` and `branch` (null when not triggered by a branch).
- `runs` never reuses a finished snapshot for longer than `--ttl`, so a new run or a moved branch tip is picked up.
