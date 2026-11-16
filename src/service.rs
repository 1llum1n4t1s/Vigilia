//! 「キャッシュ → (必要なら) GraphQL」の取得を 1 か所にまとめる層。
//!
//! `status` / `watch` はどちらもここを通るため、複数セッションが同じ対象を
//! 見ているときは TTL 内の取得が 1 回に集約される。

use crate::cache::{self, Cache};
use crate::error::{Result, VigiliaError};
use crate::github::Client;
use crate::model::Snapshot;
use crate::target::Target;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// GitHub GraphQL から取得した。
    Api,
    /// 新鮮な共有キャッシュを使った。
    Cache,
    /// レート制限のため、古いキャッシュで代替した。
    StaleCache,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Api => "api",
            Source::Cache => "cache",
            Source::StaleCache => "stale-cache",
        }
    }
}

pub struct Fetched {
    pub snapshot: Snapshot,
    pub source: Source,
    /// 取得からの経過(API 直取得は 0)。
    pub age: Duration,
    /// `StaleCache` のとき、GitHub が示した再試行までの待ち時間(`Retry-After` 等)。
    /// 呼び出し側(watch)はこの間 API を叩かないこと。
    pub retry_after: Option<Duration>,
}

pub struct Resolver {
    repo: Option<(String, String)>,
    target: Target,
    cache: Option<Cache>,
    ttl: Duration,
    /// トークン取得(`gh auth token`)はキャッシュ命中時には不要なため遅延生成する。
    client: Option<Client>,
}

impl Resolver {
    pub fn new(
        repo: Option<(String, String)>,
        target: Target,
        use_cache: bool,
        ttl: Duration,
    ) -> Self {
        let cache = if use_cache {
            match cache::default_dir().and_then(Cache::new) {
                Ok(c) => Some(c),
                Err(e) => {
                    // キャッシュは最適化なので、使えなくても継続する。
                    eprintln!("警告: キャッシュを使えません({e})。直接取得します");
                    None
                }
            }
        } else {
            None
        };
        Resolver {
            repo,
            target,
            cache,
            ttl,
            client: None,
        }
    }

    /// キャッシュのキー(repo + 対象)。run id はグローバルに一意なので repo を含めない。
    fn key(&self) -> String {
        match &self.target {
            Target::Run(id) => format!("run__{id}"),
            Target::Sha(sha) => {
                let (o, r) = self.repo.as_ref().expect("sha 対象には repo が必須");
                format!("{}__{}__sha__{sha}", o.to_lowercase(), r.to_lowercase())
            }
        }
    }

    fn fetch_api(&mut self) -> Result<Snapshot> {
        if self.client.is_none() {
            self.client = Some(Client::new()?);
        }
        self.client
            .as_ref()
            .expect("直前で初期化済み")
            .fetch_snapshot(self.repo.as_ref(), &self.target)
    }

    pub fn get(&mut self) -> Result<Fetched> {
        let Some(key) = self.cache.as_ref().map(|_| self.key()) else {
            let snapshot = self.fetch_api()?;
            return Ok(Fetched {
                snapshot,
                source: Source::Api,
                age: Duration::ZERO,
                retry_after: None,
            });
        };
        let (hit, guard) = self
            .cache
            .as_ref()
            .expect("key が Some なら cache も Some")
            .get_or_lock(&key, self.ttl);
        if let Some(entry) = hit {
            let age = entry.age(cache::now_ms());
            return Ok(Fetched {
                snapshot: entry.snapshot,
                source: Source::Cache,
                age,
                retry_after: None,
            });
        }
        // ここへ来たら自分が取得役(ロックは guard が保持。Drop で解放)。
        let result = self.fetch_api();
        let cache = self.cache.as_ref().expect("cache あり");
        let out = match result {
            Ok(snapshot) => {
                if let Err(e) = cache.write(&key, &snapshot) {
                    eprintln!("警告: キャッシュへ書き込めません({e})");
                }
                Ok(Fetched {
                    snapshot,
                    source: Source::Api,
                    age: Duration::ZERO,
                    retry_after: None,
                })
            }
            // レート制限時は、古くても共有キャッシュがあればそれを返して継続できるようにする。
            // 待ち時間は捨てずに返し、呼び出し側が API を叩かず待てるようにする。
            Err(VigiliaError::RateLimited { retry_after, message }) => match cache.read(&key) {
                Some(entry) => {
                    let age = entry.age(cache::now_ms());
                    Ok(Fetched {
                        snapshot: entry.snapshot,
                        source: Source::StaleCache,
                        age,
                        retry_after: Some(retry_after.unwrap_or_default()),
                    })
                }
                None => Err(VigiliaError::RateLimited { retry_after, message }),
            },
            Err(e) => Err(e),
        };
        drop(guard);
        out
    }
}
