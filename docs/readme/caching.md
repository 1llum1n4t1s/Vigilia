# Caching

Back to [README](../../README.md).

- Location: Windows `%LOCALAPPDATA%\Vigilia\cache`, macOS `~/Library/Caches/Vigilia/cache`, Linux `$XDG_CACHE_HOME/Vigilia/cache`. Override it with the `VIGILIA_CACHE_DIR` environment variable.
- One file per key (repository plus target). Files are written to a temporary file and renamed, so a reader never sees a half-written entry.
- A snapshot is reused only within `--ttl` (15 seconds by default), even if it has finished, because a re-run changes the result. The exception is while rate limited: a stale cache may stand in (see [Rate limits](rate-limits.md)). `--no-cache` disables the cache.
- The cache is an optimization, not a source of truth. Corrupt, outdated or unreadable files are ignored and refetched.
