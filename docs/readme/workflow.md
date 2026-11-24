# Workflow filters (`--workflow` / `--event`)

Back to [Commands](commands.md).

By default a SHA target waits for **every** workflow of the commit (including Dependabot's). Add `--workflow` to `watch` / `status` to conclude from the runs of one workflow only; `--event` narrows it further (`push`, `workflow_dispatch`, ...). They are only valid for SHA / `HEAD` targets, not for a run id.

- `--workflow` takes the **display name** (`Release`), the **file path** (`.github/workflows/release.yml`) or the **file name** (`release.yml`). Matching is exact and case sensitive; there is no partial match. `--event` is case insensitive.
- Until a matching run exists, `watch` waits (up to `--wait-for-runs`, then exit 7) and `status` exits 7.
- **Several matching runs.** Runs are grouped by (workflow, event) and only the **newest** run of each group (by creation time, then run number) counts: an older failed run that was superseded does not hide a newer success. Runs with **different events** (for example one `push` and one `workflow_dispatch` of the same workflow) are separate groups and **all** count, with the usual failure > cancelled > success order; add `--event` to look at only one. A re-run keeps the same run id and increases its attempt, so it is one run.
- **Ambiguity.** If the value matches more than one distinct workflow (for example two files that share the display name `CI`), `watch` / `status` stop with exit code 2 and list the candidates; specify the file path instead.
- Dynamic workflows (such as Dependabot Updates) have no file path, so only their display name can select them.
- The cache stores the whole commit and does not depend on the filters, so sessions using different filters share it safely. A finished commit is reused for up to 5 minutes only when the *filtered* result is finished, so a workflow that has not appeared yet is never hidden by a stale snapshot.
