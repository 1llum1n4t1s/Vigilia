//! 「キャッシュ → (必要なら) GraphQL」の取得を 1 か所にまとめる層。
//!
//! `status` / `watch` / `runs` はいずれもここを通るため、複数セッションが同じ対象を
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

pub struct Resolver {
    repo: Option<(String, String)>,
    target: Target,
    cache: Option<Cache>,
    ttl: Duration,
    /// 指定があれば、取得結果(絞り込む前の全体。キャッシュは条件に依存しない)を絞り込んで返す。
    filter: Option<RunFilter>,
    /// トークン取得(`gh auth token`)はキャッシュ命中時には不要なため遅延生成する。
    client: Option<Client>,
}

/// 文字列の FNV-1a 64 ビットハッシュ(キャッシュキーの区別用。暗号用途ではない)。
fn fnv1a(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// run 対象で `--repo` が指定されているとき、run の実際のリポジトリと一致するか検査する。
/// (SHA・ブランチ対象は repo をクエリに含めて取得するため、不一致は起こらない。)
fn check_repo(
    repo: Option<&(String, String)>,
    target: &Target,
    snap: &Snapshot,
) -> Result<()> {
    if let (Target::Run(id), Some((o, n))) = (target, repo) {
        let want = format!("{o}/{n}");
        if !snap.repo.eq_ignore_ascii_case(&want) {
            return Err(VigiliaError::Usage(format!(
                "run {id} は {} のものです(--repo {want} と一致しません)",
                snap.repo
            )));
        }
    }
    Ok(())
}

impl Resolver {
    pub fn new(
        repo: Option<(String, String)>,
        target: Target,
        use_cache: bool,
        ttl: Duration,
        filter: Option<RunFilter>,
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
            filter,
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

    /// API から取得する。トークン取得(`gh auth token`)を伴う Client は初回だけ作る。
    fn fetch_api(
        client: &mut Option<Client>,
        repo: Option<&(String, String)>,
        target: &Target,
    ) -> Result<Snapshot> {
        let client = match client {
            Some(c) => c,
            None => client.insert(Client::new()?),
        };
        client.fetch_snapshot(repo, target)
    }

    /// 取得して返す。絞り込み条件があるときは結果も絞り込んで返す
    /// (workflow が曖昧なら使用エラー)。
    ///
    /// 取得元(キャッシュ・古いキャッシュ・API)によらず、run と `--repo` の不一致を必ず検出する。
    /// run id のキャッシュキーは repo を含まないため、他リポジトリの run を `--repo` 付きで
    /// 引いてもキャッシュが命中し得る。
    pub fn get(&mut self) -> Result<Fetched> {
        let mut fetched = self.fetch_or_cached()?;
        check_repo(self.repo.as_ref(), &self.target, &fetched.snapshot)?;
        if let Some(f) = &self.filter {
            fetched.snapshot = fetched.snapshot.filtered(f)?;
        }
        Ok(fetched)
    }

    fn fetch_or_cached(&mut self) -> Result<Fetched> {
        let Some(cache) = &self.cache else {
            let snapshot = Self::fetch_api(&mut self.client, self.repo.as_ref(), &self.target)?;
            return Ok(Fetched::from_api(snapshot));
        };
        let key = self.key();
        let (hit, guard) = cache.get_or_lock(&key, self.ttl);
        if let Some(entry) = hit {
            return Ok(Fetched::from_cache(entry, Source::Cache, None));
        }
        // ここへ来たら自分が取得役(ロックは guard が保持し、書き込みと古い読み出しの後に解放する)。
        let result = Self::fetch_api(&mut self.client, self.repo.as_ref(), &self.target);
        let out = match result {
            Ok(snapshot) => {
                if let Err(e) = cache.write(&key, &snapshot) {
                    eprintln!("警告: キャッシュへ書き込めません({e})");
                }
                Ok(Fetched::from_api(snapshot))
            }
            // レート制限時は、古くても共有キャッシュがあればそれを返して継続できるようにする。
            // 待ち時間は捨てずに返し、呼び出し側が API を叩かず待てるようにする。
            Err(VigiliaError::RateLimited { retry_after, message }) => match cache.read(&key) {
                Some(entry) => Ok(Fetched::from_cache(
                    entry,
                    Source::StaleCache,
                    Some(retry_after.unwrap_or_default()),
                )),
                None => Err(VigiliaError::RateLimited { retry_after, message }),
            },
            Err(e) => Err(e),
        };
        drop(guard);
        out
    }
}

impl Fetched {
    /// API から取得した結果。
    fn from_api(snapshot: Snapshot) -> Self {
        Fetched {
            snapshot,
            source: Source::Api,
            age: Duration::ZERO,
            retry_after: None,
        }
    }

    /// キャッシュ(新鮮・古いもの)の内容。
    fn from_cache(entry: cache::Entry, source: Source, retry_after: Option<Duration>) -> Self {
        let age = entry.age(cache::now_ms());
        Fetched {
            snapshot: entry.snapshot,
            source,
            age,
            retry_after,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Suite;

    fn resolver(target: Target, filter: Option<RunFilter>) -> Resolver {
        Resolver::new(
            Some(("O".into(), "R".into())),
            target,
            false,
            Duration::from_secs(15),
            filter,
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

    /// キャッシュ命中経路(API を呼ばない)を作る。client は None のままなので、
    /// 万一 API 経路へ進めば `gh` を起動しようとする(= テストが誤って成功しない)。
    fn cached_resolver(name: &str, repo: (&str, &str), snap: &Snapshot) -> Resolver {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test-tmp")
            .join(format!("svc-{name}-{}-{}", std::process::id(), cache::now_ms()));
        let cache = Cache::new(dir).unwrap();
        cache.write("run__123456789", snap).unwrap();
        Resolver {
            repo: Some((repo.0.into(), repo.1.into())),
            target: Target::Run(123456789),
            cache: Some(cache),
            ttl: Duration::from_secs(60),
            filter: None,
            client: None,
        }
    }

    #[test]
    fn キャッシュ命中でもrepoの不一致を検出する() {
        // snapshot の repo は "o/r"。--repo が別リポジトリなら、キャッシュ命中でも使用エラー。
        let snap = done_snapshot(".github/workflows/ci.yml");
        let mut r = cached_resolver("mismatch", ("other", "x"), &snap);
        match r.get() {
            Err(VigiliaError::Usage(m)) => assert!(m.contains("o/r") && m.contains("other/x"), "{m}"),
            other => panic!("使用エラーのはず: {:?}", other.map(|f| f.source)),
        }
        // 大文字小文字だけの違いは一致扱い。
        let mut r = cached_resolver("match", ("O", "R"), &snap);
        let f = r.get().expect("一致すれば通る");
        assert_eq!(f.source, Source::Cache);
    }

    #[test]
    fn キャッシュキーは対象ごとに別でフィルタ条件を含まない() {
        let sha = resolver(Target::Sha("abc1234".into()), None).key();
        assert_eq!(sha, "o__r__sha__abc1234");
        let f = RunFilter {
            workflow: Some("ci.yml".into()),
            ..Default::default()
        };
        // 絞り込み条件が違っても同じキー(取得結果は条件に依存しないので共有してよい)。
        assert_eq!(
            resolver(Target::Sha("abc1234".into()), Some(f)).key(),
            sha
        );
        // ファイル名で `/` と `_` が潰れるブランチ名でも衝突しない。
        let a = resolver(Target::Branch("release/1".into()), None).key();
        let b = resolver(Target::Branch("release_1".into()), None).key();
        assert_ne!(cache::sanitize_key(&a), cache::sanitize_key(&b));
        assert_ne!(a, sha);
    }
}
