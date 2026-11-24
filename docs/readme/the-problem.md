# The problem

Back to [README](../../README.md).

When several AI agent sessions (or scripts) run under the same GitHub account and poll with `gh run watch` or `gh run view`, they all draw from **one REST quota per authenticated user (5,000 requests per hour)**. Once it is exhausted, every session fails with `HTTP 403: API rate limit exceeded`, and waiting does not make the total faster. Vigilia addresses this in two ways:

1. **It uses GraphQL only.** The GraphQL quota (5,000 points per hour) is counted separately from REST and is normally almost unused. `checkSuites` / `workflowRun` / `checkRuns` are combined into one query that costs a few points per request. Vigilia keeps working when the REST quota is 0: it only calls `POST /graphql` and `GET /rate_limit`, which does not consume quota.
2. **It shares results.** Fetched snapshots are stored in a local cache. While a snapshot is fresh (TTL 15 seconds by default), other sessions watching the same target get the cached copy instead of querying again, and concurrent fetches are collapsed into one with a lightweight lock. There is no daemon; sessions coordinate through files only.
