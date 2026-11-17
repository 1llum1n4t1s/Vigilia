//! セッション間で共有するファイルキャッシュ。
//!
//! 目的: 複数の Claude Code セッション(別プロセス)が同じ run を見張っても、
//! GitHub への問い合わせを 1 回にまとめる。常駐デーモンは持たず、ファイルだけで協調する。
//!
//! 設計:
//! - 1 キー(repo + 対象)= 1 ファイル。中身は取得時刻(epoch ミリ秒)と `Snapshot`。
//! - 書き込みは「同一ディレクトリの一時ファイルへ書いてから rename」でアトミックに置換する。
//!   読み手は旧版か新版のどちらかを必ず読む(途中状態は見えない)。
//! - Windows では rename 置換の瞬間に読み取りが共有違反で失敗し得るため、
//!   読み取り失敗・JSON 破損はすべて「キャッシュなし」として扱う(キャッシュは最適化であり正本ではない)。
//! - 同時取得の重複を避けるため、キーごとの軽量ロックファイル(`create_new`)を使う。
//!   ロックは助言的で、古いロック(一定時間超過)は無視して奪う。ロックが壊れても
//!   結果は「重複して取得する」だけで、正しさには影響しない。

use crate::error::{Result, VigiliaError};
use crate::model::Snapshot;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 完了済み(結果が変わらない)スナップショットを再利用してよい時間。
/// 再実行(re-run)で変わり得るため無期限にはしない。
pub const TERMINAL_TTL: Duration = Duration::from_secs(300);

/// ロックを「古い」とみなす経過時間。異常終了などで残ったロックを奪う頻度の目安であり、
/// 重複取得を完全に防ぐ値ではない。待ち手は `LOCK_WAIT`(8 秒)で待つのをやめて自分で取得に
/// 進むため、取得が `LOCK_WAIT` を超えて長引けば(通信の再試行など)、この値をいくら長くしても
/// 重複取得は起き得る。それは DESIGN で許容している(正しさには影響しない)。長くすると、
/// 残骸のロックに対して待ち手が毎回 `LOCK_WAIT` 待つ時間を延ばすだけなので 60 秒に留める。
/// 奪われた後に元の保持者が解放しても、トークン照合により相手のロックは消えない。
const LOCK_STALE: Duration = Duration::from_secs(60);

/// 他セッションの取得完了を待つ最大時間。
const LOCK_WAIT: Duration = Duration::from_secs(8);

#[derive(Debug, Serialize, Deserialize)]
pub struct Entry {
    /// 形式バージョン。互換性が無い版は無視する。
    pub version: u32,
    /// 取得時刻(UNIX epoch ミリ秒)。
    pub fetched_at_ms: u64,
    pub snapshot: Snapshot,
}

/// v2: `Suite` に workflow_path / created_at / run_attempt / branch を追加(runs の絞り込みに必要)。
const ENTRY_VERSION: u32 = 2;

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Entry {
    /// 取得からの経過時間(時計が戻っていれば 0)。
    pub fn age(&self, now_ms: u64) -> Duration {
        Duration::from_millis(now_ms.saturating_sub(self.fetched_at_ms))
    }

    /// `ttl` 以内、または完了済みで `TERMINAL_TTL` 以内なら新鮮とみなす。
    #[cfg(test)]
    pub fn is_fresh(&self, now_ms: u64, ttl: Duration) -> bool {
        self.is_fresh_with(now_ms, ttl, &|s| s.overall().is_terminal())
    }

    /// `is_fresh` の一般形。「完了済みとみなして TERMINAL_TTL まで再利用してよいか」を
    /// 呼び出し側が決める。workflow で絞った監視や `runs` では、絞る前の全体が完了していても
    /// 「待っている run がまだ現れていない」だけの場合があり、古い結果を使い回すと見逃すため。
    pub fn is_fresh_with(
        &self,
        now_ms: u64,
        ttl: Duration,
        terminal: &dyn Fn(&Snapshot) -> bool,
    ) -> bool {
        let age = self.age(now_ms);
        if age <= ttl {
            return true;
        }
        terminal(&self.snapshot) && age <= TERMINAL_TTL.max(ttl)
    }
}

pub struct Cache {
    dir: PathBuf,
}

/// ロックの解放(Drop で削除)。
pub struct LockGuard {
    path: PathBuf,
    /// 作成時にロックファイルへ書いた一意の値(プロセス ID・時刻・通番。秘密ではない)。
    /// 古いと判断されて他セッションに奪われた後に、相手のロックを誤って消さないための照合用。
    token: String,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        // 中身が自分のトークンと一致するときだけ削除する。奪われた(別のトークンに置き換わった)
        // ロックは相手のものなので触らない。読めない場合も削除しない
        // (残っても次回の古いロック判定で回収されるため無視してよい)。
        // 照合から削除までの間に奪われる競合は残るが、その場合も「重複して取得する」だけで済む。
        if fs::read_to_string(&self.path).is_ok_and(|c| c == self.token) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// ロックごとに一意で、秘密ではないトークンを作る(プロセス ID + ナノ秒 + プロセス内通番)。
fn new_lock_token() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

/// キーとして安全なファイル名へ変換する(英数字と `.-_` 以外は `_`)。
pub fn sanitize_key(key: &str) -> String {
    key.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// 既定のキャッシュディレクトリ。
/// `VIGILIA_CACHE_DIR` があればそれを優先(テスト・検証用)。
/// Windows: `%LOCALAPPDATA%\Vigilia\cache` / macOS: `~/Library/Caches/Vigilia/cache` /
/// Linux: `$XDG_CACHE_HOME/Vigilia/cache`。
pub fn default_dir() -> Result<PathBuf> {
    if let Some(v) = std::env::var_os("VIGILIA_CACHE_DIR")
        && !v.is_empty()
    {
        return Ok(PathBuf::from(v));
    }
    let base = dirs::cache_dir()
        .ok_or_else(|| VigiliaError::Io("キャッシュディレクトリを決定できません".into()))?;
    Ok(base.join("Vigilia").join("cache"))
}

impl Cache {
    pub fn new(dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&dir)
            .map_err(|e| VigiliaError::Io(format!("{}: {e}", dir.display())))?;
        Ok(Cache { dir })
    }

    fn entry_path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{}.json", sanitize_key(key)))
    }

    fn lock_path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{}.lock", sanitize_key(key)))
    }

    /// 読み取り。存在しない/壊れている/版違いはすべて None。
    pub fn read(&self, key: &str) -> Option<Entry> {
        let bytes = fs::read(self.entry_path(key)).ok()?;
        let entry: Entry = serde_json::from_slice(&bytes).ok()?;
        (entry.version == ENTRY_VERSION).then_some(entry)
    }

    /// アトミックに書き込む(一時ファイル → rename)。
    pub fn write(&self, key: &str, snapshot: &Snapshot) -> Result<()> {
        let entry = Entry {
            version: ENTRY_VERSION,
            fetched_at_ms: now_ms(),
            snapshot: snapshot.clone(),
        };
        let json = serde_json::to_vec(&entry)
            .map_err(|e| VigiliaError::Io(format!("キャッシュのシリアライズ失敗: {e}")))?;
        let target = self.entry_path(key);
        // プロセス ID + ナノ秒 + スレッド ID で一時名を一意にし、同時書き込み同士が衝突しないようにする。
        let tmp = self.dir.join(format!(
            ".tmp-{}-{}-{:?}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
            std::thread::current().id()
        ));
        let io = |e: std::io::Error| VigiliaError::Io(format!("{}: {e}", tmp.display()));
        // 作成・書き込みのどこで失敗しても一時ファイルを残さない(ディスクフル等で
        // 途中まで書けたファイルがキャッシュ置き場に溜まり続けるのを防ぐ)。
        let written = (|| -> std::io::Result<()> {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(&json)?;
            f.sync_all().ok();
            Ok(())
        })();
        if let Err(e) = written {
            let _ = fs::remove_file(&tmp);
            return Err(io(e));
        }
        // Windows の rename は既存ファイルを置換する。読み手が掴んでいて失敗した場合は短くリトライ。
        let mut last = None;
        for _ in 0..12 {
            match fs::rename(&tmp, &target) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    last = Some(e);
                    std::thread::sleep(Duration::from_millis(30));
                }
            }
        }
        let _ = fs::remove_file(&tmp);
        Err(VigiliaError::Io(format!(
            "{}: {}",
            target.display(),
            last.map(|e| e.to_string()).unwrap_or_default()
        )))
    }

    /// ロック取得を試みる。他者が保持中なら None。古いロックは奪う。
    pub fn try_lock(&self, key: &str) -> Option<LockGuard> {
        let path = self.lock_path(key);
        for _ in 0..2 {
            match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut f) => {
                    let token = new_lock_token();
                    if f.write_all(token.as_bytes()).is_err() {
                        // 中身を書けないロックは照合できず解放もできないため、作り直さず
                        // 取り除いて「ロック無し」で進める(重複取得を許容)。
                        drop(f);
                        let _ = fs::remove_file(&path);
                        return None;
                    }
                    return Some(LockGuard { path, token });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if lock_is_stale(&path) {
                        let _ = fs::remove_file(&path);
                        continue;
                    }
                    return None;
                }
                // ロックが作れない環境(権限等)では、ロック無しで進める(重複取得を許容)。
                Err(_) => return None,
            }
        }
        None
    }

    /// 新鮮なキャッシュを返す。無ければ他セッションの取得を待って再確認する。
    /// 戻り値の第 2 要素は、呼び出し側が取得を行うべき場合の保持ロック。
    pub fn get_or_lock(
        &self,
        key: &str,
        ttl: Duration,
        terminal: &dyn Fn(&Snapshot) -> bool,
    ) -> (Option<Entry>, Option<LockGuard>) {
        if let Some(e) = self.read(key)
            && e.is_fresh_with(now_ms(), ttl, terminal)
        {
            return (Some(e), None);
        }
        if let Some(guard) = self.try_lock(key) {
            // ロック取得の間に他セッションが書き込んだ可能性があるため再確認(ダブルチェック)。
            if let Some(e) = self.read(key)
                && e.is_fresh_with(now_ms(), ttl, terminal)
            {
                return (Some(e), None);
            }
            return (None, Some(guard));
        }
        // 他セッションが取得中。完了(キャッシュ更新)を待つ。
        let deadline = std::time::Instant::now() + LOCK_WAIT;
        while std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(250));
            if let Some(e) = self.read(key)
                && e.is_fresh_with(now_ms(), ttl, terminal)
            {
                return (Some(e), None);
            }
            if !self.lock_path(key).exists() {
                break;
            }
        }
        // 待っても更新されなかった(相手が失敗した等)。自分で取得する。
        (None, self.try_lock(key))
    }
}

fn lock_is_stale(path: &Path) -> bool {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .map(|age| age > LOCK_STALE)
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Suite;
    use std::sync::Arc;

    /// テスト専用ディレクトリ(リポジトリ内 target 配下。C の TEMP は使わない)。
    fn test_dir(name: &str) -> PathBuf {
        let d = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test-tmp")
            .join(format!("{name}-{}-{}", std::process::id(), now_ms()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn snap(status: &str, conclusion: Option<&str>) -> Snapshot {
        Snapshot {
            repo: "o/r".into(),
            sha: "abc".into(),
            suites: vec![Suite {
                app: "GitHub Actions".into(),
                workflow: Some("CI".into()),
                run_id: Some(1),
                run_number: Some(1),
                event: None,
                url: None,
                status: status.into(),
                conclusion: conclusion.map(String::from),
                jobs: vec![],
                ..Default::default()
            }],
            truncated: false,
            suites_truncated: false,
            cost: None,
        }
    }

    #[test]
    fn 書いて読める_キーは安全化される() {
        let c = Cache::new(test_dir("rw")).unwrap();
        assert!(c.read("o/r:abc").is_none());
        c.write("o/r:abc", &snap("IN_PROGRESS", None)).unwrap();
        let e = c.read("o/r:abc").unwrap();
        assert_eq!(e.snapshot.repo, "o/r");
        assert_eq!(sanitize_key("o/r:abc"), "o_r_abc");
    }

    #[test]
    fn ttlの新鮮さ判定() {
        let e = Entry {
            version: 1,
            fetched_at_ms: 1_000_000,
            snapshot: snap("IN_PROGRESS", None),
        };
        let ttl = Duration::from_secs(15);
        assert!(e.is_fresh(1_000_000 + 14_000, ttl));
        assert!(!e.is_fresh(1_000_000 + 16_000, ttl));
        // 時計が戻っていても新鮮扱い(age=0)。
        assert!(e.is_fresh(999_000, ttl));
        // 完了済みは TTL を超えても TERMINAL_TTL までは新鮮。
        let done = Entry {
            version: 1,
            fetched_at_ms: 1_000_000,
            snapshot: snap("COMPLETED", Some("SUCCESS")),
        };
        assert!(done.is_fresh(1_000_000 + 200_000, ttl));
        assert!(!done.is_fresh(1_000_000 + 301_000, ttl));
    }

    #[test]
    fn 壊れたファイルは無視される() {
        let dir = test_dir("corrupt");
        let c = Cache::new(dir.clone()).unwrap();
        fs::write(dir.join("k.json"), b"{ not json").unwrap();
        assert!(c.read("k").is_none());
        // 版違いも無視。
        fs::write(
            dir.join("v.json"),
            br#"{"version":99,"fetched_at_ms":1,"snapshot":null}"#,
        )
        .unwrap();
        assert!(c.read("v").is_none());
    }

    #[test]
    fn ロックは排他で_古いロックは奪える() {
        let dir = test_dir("lock");
        let c = Cache::new(dir.clone()).unwrap();
        let g = c.try_lock("k").expect("最初は取得できる");
        assert!(c.try_lock("k").is_none(), "保持中は取得できない");
        drop(g);
        assert!(c.try_lock("k").is_some(), "解放後は取得できる");
        // 古いロックの再現: 更新時刻を過去へ。
        let p = dir.join("old.lock");
        let f = fs::File::create(&p).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(600))
            .unwrap();
        drop(f);
        assert!(c.try_lock("old").is_some(), "古いロックは奪える");
    }

    #[test]
    fn 奪われたロックは解放時に消さない() {
        let dir = test_dir("stolen");
        let c = Cache::new(dir.clone()).unwrap();
        let g = c.try_lock("k").expect("取得できる");
        let p = dir.join("k.lock");
        // 自分のロックにはトークンが書かれている。
        assert!(!fs::read_to_string(&p).unwrap().is_empty());
        // 古いと判断されて他セッションに奪われた状態を再現(別のトークンで置き換わる)。
        fs::remove_file(&p).unwrap();
        fs::write(&p, "other-session-token").unwrap();
        drop(g);
        assert_eq!(
            fs::read_to_string(&p).unwrap(),
            "other-session-token",
            "相手のロックを消してはいけない"
        );
        // 自分のロックなら従来どおり解放で消える。
        let _ = fs::remove_file(&p);
        let g = c.try_lock("k").expect("再取得できる");
        drop(g);
        assert!(!p.exists());
    }

    #[test]
    fn ロックの古さ判定は待ち手の待機より長い() {
        // 待ち手が LOCK_WAIT 待つ間に、生きている取得役のロックを古いと誤判定して奪わないこと。
        assert!(LOCK_WAIT < LOCK_STALE);
    }

    #[test]
    fn 新鮮ならロックせずキャッシュを返す() {
        let c = Cache::new(test_dir("fresh")).unwrap();
        c.write("k", &snap("IN_PROGRESS", None)).unwrap();
        let (hit, guard) = c.get_or_lock("k", Duration::from_secs(15), &|s| s.overall().is_terminal());
        assert!(hit.is_some());
        assert!(guard.is_none());
        // 古い(ttl=0)場合はロック付きで取得を促す。
        std::thread::sleep(Duration::from_millis(5));
        let (hit, guard) = c.get_or_lock("k", Duration::from_millis(0), &|s| s.overall().is_terminal());
        assert!(hit.is_none());
        assert!(guard.is_some());
    }

    #[test]
    fn 同時書き込みでも読み手は壊れた内容を見ない() {
        let c = Arc::new(Cache::new(test_dir("concurrent")).unwrap());
        c.write("k", &snap("IN_PROGRESS", None)).unwrap();
        let mut handles = Vec::new();
        for i in 0..4 {
            let c = Arc::clone(&c);
            handles.push(std::thread::spawn(move || {
                for n in 0..40 {
                    let s = if (i + n) % 2 == 0 {
                        snap("IN_PROGRESS", None)
                    } else {
                        snap("COMPLETED", Some("SUCCESS"))
                    };
                    c.write("k", &s).unwrap();
                }
            }));
        }
        // 書き込み中に読み続け、読めたなら必ず整合した内容であること。
        // (Windows では置換の瞬間に None になり得るが、壊れた Some は無い)
        let reader = {
            let c = Arc::clone(&c);
            std::thread::spawn(move || {
                let mut ok = 0;
                for _ in 0..400 {
                    if let Some(e) = c.read("k") {
                        assert_eq!(e.snapshot.repo, "o/r");
                        ok += 1;
                    }
                }
                ok
            })
        };
        for h in handles {
            h.join().unwrap();
        }
        assert!(reader.join().unwrap() > 0);
        assert!(c.read("k").is_some());
    }
}
