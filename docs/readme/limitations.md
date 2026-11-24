# Limitations

Back to [README](../../README.md).

- Resolving a run id to a run uses the legacy global ID format that GitHub has deprecated (`011:WorkflowRun<id>`), because it is the only way to look up a run by id without REST. If it is removed in the future, only run id targets will fail with an API error; SHA targets are unaffected.
- At most 50 check suites per commit and 30 check runs per suite (50 for a run id target) are fetched. If suites exceed the limit, missed suites could change the conclusion, so success, cancelled and "no run" are reported as an error (exit code 2) instead of being claimed. A failure is still reported without a filter, but after `--workflow` / `--event` it is also an error (a newer run may have been missed). `runs` still exits 0 when a matching run is found and prints a note to stderr when suites were missed; with no match it is an error (exit code 2), not 7. Exceeding the job limit is shown as partially omitted.
- Check suites of non-Actions apps that have no check runs may never complete, so they are excluded from watching.
