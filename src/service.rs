//! 「キャッシュ → (必要なら) GraphQL」の取得を 1 か所にまとめる層。
//!
//! `status` / `watch` はどちらもここを通るため、複数セッションが同じ対象を
//! 見ているときは TTL 内の取得が 1 回に集約される。

use crate::cache::{self, Cache};
use crate::error::{Result, VigiliaError};
use crate::github::Client;
use crate::model::{RunFilter, Snapshot};
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

/// キャッシュを「完了済みだから TERMINAL_TTL まで使い回してよい」と判断する方針。
/// 取得結果そのもの(絞り込む前の全体)はどの方針でも共通で、キャッシュは条件に依存しない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Freshness {
    /// 全 suite の結論が確定していれば使い回す(絞り込みなしの `watch` / `status`)。
    Default,
    /// 絞り込んだ後の結論が確定しているときだけ使い回し、取得結果も絞り込んで返す。
    /// 絞る前の全体が完了済みでも、待っている run が未出現なだけの場合に古い結果で見逃さないため。
    Filtered(RunFilter),
    /// 完了済みでも TERMINAL_TTL を適用しない(`runs`。新しい run やブランチ先頭の移動を見逃さない)。
    Strict,
}

pub struct Resolver {
    repo: Option<(String, String)>,
    target: Target,
    cache: Option<Cache>,
    ttl: Duration,
    freshness: Freshness,
    /// トークン取得(`gh auth token`)はキャッシュ命中時には不要なため遅延生成する。
    client: Option<Client>,
}

/// 文字列の FNV-1a 64 ビットハッシュ(キャッシュキーの区別用。暗号用途ではない)。
fn fnv1a(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

impl Resolver {
    pub fn new(
        repo: Option<(String, String)>,
        target: Target,
        use_cache: bool,
        ttl: Duration,
        freshness: Freshness,
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
            freshness,
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
            // ブランチ名は `/` などがファイル名で `_` に潰れて衝突し得るため、ハッシュを添える。
            // (絞り込み条件はキーに含めない。取得結果は条件に依存しないため。)
            Target::Branch(b) => {
                let (o, r) = self.repo.as_ref().expect("ブランチ対象には repo が必須");
                format!(
                    "{}__{}__branch__{b}__{:016x}",
                    o.to_lowercase(),
                    r.to_lowercase(),
                    fnv1a(b)
                )
            }
        }
    }

    /// 完了済みとして TERMINAL_TTL まで再利用してよいか(`Freshness` に従う)。
    fn is_terminal(&self, snap: &Snapshot) -> bool {
        match &self.freshness {
            Freshness::Default => snap.overall().is_terminal(),
            Freshness::Filtered(f) => snap
                .filtered(f)
                .map(|s| s.overall().is_terminal())
                .unwrap_or(false),
            Freshness::Strict => false,
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

    /// 取得して返す。`Freshness::Filtered` のときは結果も絞り込んで返す
    /// (workflow が曖昧なら使用エラー)。
    pub fn get(&mut self) -> Result<Fetched> {
        let mut fetched = self.get_unfiltered()?;
        if let Freshness::Filtered(f) = &self.freshness {
            fetched.snapshot = fetched.snapshot.filtered(f)?;
        }
        Ok(fetched)
    }

    fn get_unfiltered(&mut self) -> Result<Fetched> {
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
            .get_or_lock(&key, self.ttl, &|s| self.is_terminal(s));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Suite;

    fn resolver(target: Target, freshness: Freshness) -> Resolver {
        Resolver::new(
            Some(("O".into(), "R".into())),
            target,
            false,
            Duration::from_secs(15),
            freshness,
        )
    }

    fn done_snapshot(workflow_path: &str) -> Snapshot {
        Snapshot {
            repo: "o/r".into(),
            sha: "abc".into(),
            suites: vec![Suite {
                app: "GitHub Actions".into(),
                workflow_path: Some(workflow_path.into()),
                run_id: Some(1),
                status: "COMPLETED".into(),
                conclusion: Some("SUCCESS".into()),
                ..Default::default()
            }],
            truncated: false,
            suites_truncated: false,
            cost: None,
        }
    }

    #[test]
    fn キャッシュキーは対象ごとに別でフィルタ条件を含まない() {
        let sha = resolver(Target::Sha("abc1234".into()), Freshness::Default).key();
        assert_eq!(sha, "o__r__sha__abc1234");
        let f = RunFilter {
            workflow: Some("ci.yml".into()),
            ..Default::default()
        };
        // 絞り込み条件が違っても同じキー(取得結果は条件に依存しないので共有してよい)。
        assert_eq!(
            resolver(Target::Sha("abc1234".into()), Freshness::Filtered(f)).key(),
            sha
        );
        // ファイル名で `/` と `_` が潰れるブランチ名でも衝突しない。
        let a = resolver(Target::Branch("release/1".into()), Freshness::Strict).key();
        let b = resolver(Target::Branch("release_1".into()), Freshness::Strict).key();
        assert_ne!(cache::sanitize_key(&a), cache::sanitize_key(&b));
        assert_ne!(a, sha);
    }

    #[test]
    fn 完了済みの再利用は絞り込み後の結論で決める() {
        let snap = done_snapshot(".github/workflows/ci.yml");
        assert!(resolver(Target::Sha("abc1234".into()), Freshness::Default).is_terminal(&snap));
        // 全体は完了済みでも、待っている release.yml はまだ現れていない → 再利用しない。
        let f = RunFilter {
            workflow: Some("release.yml".into()),
            ..Default::default()
        };
        assert!(
            !resolver(Target::Sha("abc1234".into()), Freshness::Filtered(f)).is_terminal(&snap)
        );
        let f = RunFilter {
            workflow: Some("ci.yml".into()),
            ..Default::default()
        };
        assert!(
            resolver(Target::Sha("abc1234".into()), Freshness::Filtered(f)).is_terminal(&snap)
        );
        // runs(Strict)は完了済みでも長く使い回さない。
        assert!(!resolver(Target::Sha("abc1234".into()), Freshness::Strict).is_terminal(&snap));
    }
}
