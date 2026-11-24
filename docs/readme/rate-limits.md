# Rate limits

Back to [README](../../README.md).

- Requests are always sequential, and the `watch` interval is limited to 5 seconds or more.
- `Retry-After` and `x-ratelimit-reset` are honored. `watch` does not abort when it is rate limited: it waits for the indicated time and resumes (it returns exit code 5 if that would exceed `--timeout`). `status` returns exit code 5 if there is no cache, and also if only a stale cache exists (the stale snapshot is printed with a warning, but it is never reported as a conclusion).
- A GraphQL `RATE_LIMITED` error (which arrives with HTTP 200) is handled the same way. While limited, a stale cached snapshot is shown with a warning if one exists. `watch` prints it but does not conclude from it (neither a finished result nor "no run found"): it waits and fetches again.
- Network errors and 5xx responses are tried up to 3 times in total (2 retries, waiting 1 s and 2 s).
