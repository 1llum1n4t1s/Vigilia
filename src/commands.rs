//! サブコマンドの実装(watch / status / budget)。

use crate::error::{Result, VigiliaError, exit};
use crate::github::{Client, Resource};
use crate::model::{Overall, Snapshot};
use crate::render;
use crate::service::{Fetched, Resolver, Source};
use crate::target::{self, Target};
use crate::{BudgetArgs, StatusArgs, TargetArgs, WatchArgs};
use chrono::{Local, Utc};
use serde_json::json;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// 取得間隔の下限。二次レート制限と共有 API への負荷を避けるための安全弁。
const MIN_INTERVAL_SECS: u64 = 5;

/// 引数から (repo, target) を確定し、Resolver を作る。
fn build_resolver(a: &TargetArgs) -> Result<Resolver> {
    let target = target::parse_target(a.target.as_deref().unwrap_or("HEAD"))?;
    let repo = match (&a.repo, &target) {
        (Some(r), _) => Some(target::parse_repo(r)?),
        // run id は repo 不要(run から逆引きできる)。省略時は推定しない。
        (None, Target::Run(_)) => None,
        (None, Target::Sha(_)) => Some(target::infer_repo()?),
    };
    Ok(Resolver::new(
        repo,
        target,
        !a.no_cache,
        Duration::from_secs(a.ttl),
    ))
}

/// 結論 → 終了コード。
fn overall_code(o: Overall) -> i32 {
    match o {
        Overall::Success => exit::SUCCESS,
        Overall::Failure => exit::FAILURE,
        Overall::Cancelled => exit::CANCELLED,
        Overall::InProgress => exit::IN_PROGRESS,
        Overall::NoRuns => exit::NOT_FOUND,
    }
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

pub fn status(a: &StatusArgs) -> Result<i32> {
    let mut resolver = build_resolver(&a.target)?;
    let fetched = resolver.get()?;
    let overall = fetched.snapshot.overall();
    if a.json {
        let out = json!({
            "overall": overall,
            "source": fetched.source.as_str(),
            "age_secs": fetched.age.as_secs(),
            "snapshot": fetched.snapshot,
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    } else {
        print!("{}", render::render_snapshot(&fetched.snapshot, Utc::now()));
        println!("{}", source_line(&fetched));
    }
    Ok(overall_code(overall))
}

/// 取得元と GraphQL コストの 1 行表示。
fn source_line(f: &Fetched) -> String {
    let src = match f.source {
        Source::Api => "GraphQL で取得".to_string(),
        Source::Cache => format!("共有キャッシュ({}秒前)", f.age.as_secs()),
        Source::StaleCache => format!(
            "古いキャッシュ({}秒前。レート制限のため再取得できません)",
            f.age.as_secs()
        ),
    };
    match (&f.source, &f.snapshot.cost) {
        (Source::Api, Some(c)) => format!(
            "[{src} / cost {} / GraphQL 残り {} / リセット {}]",
            c.cost, c.remaining, c.reset_at
        ),
        _ => format!("[{src}]"),
    }
}

// ---------------------------------------------------------------------------
// watch
// ---------------------------------------------------------------------------

pub fn watch(a: &WatchArgs) -> Result<i32> {
    if a.interval < MIN_INTERVAL_SECS {
        return Err(VigiliaError::Usage(format!(
            "--interval は {MIN_INTERVAL_SECS} 秒以上にしてください(API 負荷と二次レート制限の回避のため)"
        )));
    }
    if a.timeout == 0 {
        return Err(VigiliaError::Usage("--timeout は 1 以上にしてください".into()));
    }
    let mut resolver = build_resolver(&a.target)?;
    let started = Instant::now();
    let timeout = Duration::from_secs(a.timeout);
    let interval = Duration::from_secs(a.interval);
    // job ごとの直近表示ラベル。変化したものだけを出力する。
    let mut seen: HashMap<String, &'static str> = HashMap::new();
    let mut header_printed = false;
    let mut stale_warned = false;

    loop {
        match resolver.get() {
            Ok(f) => {
                if f.source == Source::StaleCache && !stale_warned {
                    eprintln!(
                        "警告: レート制限のため古いキャッシュ({}秒前)で継続します",
                        f.age.as_secs()
                    );
                    stale_warned = true;
                }
                if !header_printed && !f.snapshot.suites.is_empty() {
                    println!(
                        "{} @ {} を監視します",
                        f.snapshot.repo,
                        render::short_sha(&f.snapshot.sha)
                    );
                    header_printed = true;
                }
                print_changes(&f.snapshot, &mut seen);
                let overall = f.snapshot.overall();
                if overall.is_terminal() {
                    print_final(&f.snapshot, overall);
                    return Ok(overall_code(overall));
                }
                if overall == Overall::NoRuns
                    && started.elapsed() >= Duration::from_secs(a.wait_for_runs)
                {
                    return Err(VigiliaError::NotFound(format!(
                        "{}秒待っても run が現れませんでした(ワークフローが無い/対象外の可能性)",
                        a.wait_for_runs
                    )));
                }
            }
            Err(VigiliaError::RateLimited { retry_after, message }) => {
                // 落とさずに、示された時間(分からなければ interval)だけ待って再開する。
                let wait = retry_after.unwrap_or(interval).max(interval);
                if started.elapsed() + wait >= timeout {
                    return Err(VigiliaError::RateLimited { retry_after, message });
                }
                eprintln!(
                    "レート制限({message}): {}秒待機して再開します",
                    wait.as_secs()
                );
                std::thread::sleep(wait);
                continue;
            }
            Err(e) => return Err(e),
        }

        let elapsed = started.elapsed();
        if elapsed >= timeout {
            println!("タイムアウト: {}秒経っても完了しませんでした", a.timeout);
            return Ok(exit::TIMEOUT);
        }
        std::thread::sleep(interval.min(timeout - elapsed));
    }
}

/// 状態が変わった job(と suite)だけを `[HH:MM:SS]` 付きで出力する。
fn print_changes(snap: &Snapshot, seen: &mut HashMap<String, &'static str>) {
    let ts = Local::now().format("%H:%M:%S");
    for (i, suite) in snap.suites.iter().enumerate() {
        let skey = format!("{i}/{}", suite.run_id.unwrap_or(0));
        let slabel = render::suite_label(suite);
        if seen.insert(format!("{skey}/*"), slabel) != Some(slabel) {
            println!("[{ts}] {} : {slabel}", render::suite_title(suite));
        }
        for job in &suite.jobs {
            let label = render::job_label(job);
            if seen.insert(format!("{skey}/{}", job.name), label) != Some(label) {
                println!("[{ts}]   {} : {label}", job.name);
            }
        }
    }
}

fn print_final(snap: &Snapshot, overall: Overall) {
    for line in render::failed_jobs(snap) {
        println!("失敗: {line}");
    }
    println!(
        "結論: {} ({} @ {})",
        overall.label(),
        snap.repo,
        render::short_sha(&snap.sha)
    );
}

// ---------------------------------------------------------------------------
// budget
// ---------------------------------------------------------------------------

pub fn budget(a: &BudgetArgs) -> Result<i32> {
    let limits = Client::new()?.rate_limits()?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&limits).unwrap_or_default());
        return Ok(exit::SUCCESS);
    }
    let now = Utc::now().timestamp();
    for (name, res) in [
        ("REST (core)", &limits.core),
        ("GraphQL    ", &limits.graphql),
        ("search     ", &limits.search),
    ] {
        match res {
            Some(r) => println!("{}", budget_line(name, r, now)),
            None => println!("{name}  (情報なし)"),
        }
    }
    Ok(exit::SUCCESS)
}

fn budget_line(name: &str, r: &Resource, now: i64) -> String {
    let left = (r.reset - now).max(0);
    format!(
        "{name} 残り {:>5}/{:<5}  リセット {} (あと {})",
        r.remaining,
        r.limit,
        render::fmt_reset(r.reset),
        render::fmt_secs(left)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 結論と終了コードの対応() {
        assert_eq!(overall_code(Overall::Success), 0);
        assert_eq!(overall_code(Overall::Failure), 1);
        assert_eq!(overall_code(Overall::Cancelled), 3);
        assert_eq!(overall_code(Overall::InProgress), 6);
        assert_eq!(overall_code(Overall::NoRuns), 7);
    }

    #[test]
    fn budget行の整形() {
        let r = Resource { limit: 5000, remaining: 0, used: 5000, reset: 1_000_090 };
        let line = budget_line("REST (core)", &r, 1_000_000);
        assert!(line.contains("残り     0/5000"), "{line}");
        assert!(line.contains("あと 1m30s"), "{line}");
    }
}
