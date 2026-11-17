//! サブコマンドの実装(watch / status / runs / budget)。

use crate::error::{Result, VigiliaError, exit};
use crate::github::{Client, Resource};
use crate::model::{Overall, RunFilter, RunInfo, Snapshot};
use crate::render;
use crate::service::{Fetched, Freshness, Resolver, Source};
use crate::target::{self, Target};
use crate::{BudgetArgs, RunsArgs, StatusArgs, TargetArgs, WatchArgs};
use chrono::{DateTime, Local, Utc};
use serde_json::json;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// 取得間隔の下限。二次レート制限と共有 API への負荷を避けるための安全弁。
const MIN_INTERVAL_SECS: u64 = 5;

/// 引数から (repo, target) を確定し、Resolver を作る。
fn build_resolver(a: &TargetArgs) -> Result<Resolver> {
    let target = target::parse_target(a.target.as_deref().unwrap_or("HEAD"))?;
    let filter = RunFilter {
        workflow: a.workflow.clone(),
        event: a.event.clone(),
        since: None,
    };
    if matches!(target, Target::Run(_)) && !filter.is_unfiltered() {
        return Err(VigiliaError::Usage(
            "--workflow / --event は SHA・HEAD 指定のときだけ使えます(run id は 1 つの run を指すため)"
                .into(),
        ));
    }
    let repo = match (&a.repo, &target) {
        (Some(r), _) => Some(target::parse_repo(r)?),
        // run id は repo 不要(run から逆引きできる)。省略時は推定しない。
        (None, Target::Run(_)) => None,
        (None, _) => Some(target::infer_repo()?),
    };
    let freshness = if filter.is_unfiltered() {
        Freshness::Default
    } else {
        Freshness::Filtered(filter)
    };
    Ok(Resolver::new(
        repo,
        target,
        !a.no_cache,
        Duration::from_secs(a.ttl),
        freshness,
    ))
}

/// 絞り込み条件の表示用ラベル(例: `workflow=release.yml, event=push`)。無ければ None。
fn filter_label(a: &TargetArgs) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(w) = &a.workflow {
        parts.push(format!("workflow={w}"));
    }
    if let Some(e) = &a.event {
        parts.push(format!("event={e}"));
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// `--workflow` / `--event` で絞り込んでいるか(結論の確定規則が変わる。`conclusive_overall` を参照)。
fn is_filtered(a: &TargetArgs) -> bool {
    a.workflow.is_some() || a.event.is_some()
}

/// レート制限時の待ち時間。示された時間を尊重しつつ、`--interval` より短くはしない。
fn rate_wait(interval: Duration, retry_after: Option<Duration>) -> Duration {
    retry_after.unwrap_or(interval).max(interval)
}

/// 次の取得までの待ち時間。期限までの残りが `MIN_INTERVAL_SECS` 未満なら、もう取得しない(None)。
/// 残りの端数だけ待って再取得すると、直前の取得から 5 秒未満の間隔で API を叩いてしまうため、
/// その場合は呼び出し側が期限処理(タイムアウト / 未検出)へ進む。
fn next_poll_wait(elapsed: Duration, limit: Duration, interval: Duration) -> Option<Duration> {
    let remaining = limit.saturating_sub(elapsed);
    (remaining >= Duration::from_secs(MIN_INTERVAL_SECS)).then(|| interval.min(remaining))
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

/// `status` の結論(表示用の `Overall`)と終了コード。
/// レート制限で古いキャッシュしか得られなかったときは、その断面は表示するが結論としては扱わず、
/// 終了コード 5(結論を得られなかった)にする。年齢の上限が無い古い結果で 0 / 1 / 3 / 7 を
/// 返すと、呼び出し側が古い状態を現在の結論と誤解するため。
fn status_verdict(f: &Fetched, filtered: bool) -> Result<(Overall, i32)> {
    if f.source == Source::StaleCache {
        return Ok((f.snapshot.overall(), exit::RATE_LIMITED));
    }
    let overall = f.snapshot.conclusive_overall(filtered)?;
    Ok((overall, overall_code(overall)))
}

/// `status --json` の出力。古いキャッシュ(StaleCache)のときは結論ではないので `overall` を null にし、
/// 古い断面から計算した値は追加フィールド `stale_overall` へ入れる(通常時のフィールドは変えない)。
fn status_json(f: &Fetched, overall: Overall, filter: &TargetArgs) -> serde_json::Value {
    let stale = f.source == Source::StaleCache;
    let mut out = json!({
        "overall": if stale { serde_json::Value::Null } else { json!(overall) },
        "source": f.source.as_str(),
        "age_secs": f.age.as_secs(),
        "filter": { "workflow": filter.workflow, "event": filter.event },
        "snapshot": f.snapshot,
    });
    if stale {
        out["stale_overall"] = json!(overall);
    }
    out
}

pub fn status(a: &StatusArgs) -> Result<i32> {
    let mut resolver = build_resolver(&a.target)?;
    let fetched = resolver.get()?;
    let (overall, code) = status_verdict(&fetched, is_filtered(&a.target))?;
    let stale = fetched.source == Source::StaleCache;
    if stale {
        eprintln!(
            "警告: レート制限のため古いキャッシュ({}秒前)の断面です。現在の結論を得られていません",
            fetched.age.as_secs()
        );
    }
    if a.json {
        let out = status_json(&fetched, overall, &a.target);
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    } else {
        if let Some(l) = filter_label(&a.target) {
            println!("絞り込み: {l}");
        }
        print!("{}", render::render_snapshot(&fetched.snapshot, Utc::now(), stale));
        println!("{}", source_line(&fetched));
    }
    Ok(code)
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
// runs
// ---------------------------------------------------------------------------

/// `--since` を RFC 3339 として解釈する。
fn parse_since(s: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s.trim())
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| {
            VigiliaError::Usage(format!(
                "--since は RFC 3339 形式で指定してください(例: 2026-11-18T09:00:00Z): {e}"
            ))
        })
}

pub fn runs(a: &RunsArgs) -> Result<i32> {
    if a.interval < MIN_INTERVAL_SECS {
        return Err(VigiliaError::Usage(format!(
            "--interval は {MIN_INTERVAL_SECS} 秒以上にしてください(API 負荷と二次レート制限の回避のため)"
        )));
    }
    let target = target::parse_runs_target(a.target.as_deref().unwrap_or("HEAD"))?;
    let repo = match &a.repo {
        Some(r) => target::parse_repo(r)?,
        None => target::infer_repo()?,
    };
    let filter = RunFilter {
        workflow: a.workflow.clone(),
        event: a.event.clone(),
        since: a.since.as_deref().map(parse_since).transpose()?,
    };
    // runs は「新しい run が現れたか」を見るため、完了済みでも長くキャッシュしない(Strict)。
    let mut resolver = Resolver::new(
        Some(repo),
        target,
        !a.no_cache,
        Duration::from_secs(a.ttl),
        Freshness::Strict,
    );
    let started = Instant::now();
    let wait = Duration::from_secs(a.wait);
    let interval = Duration::from_secs(a.interval);

    loop {
        match resolver.get() {
            Ok(f) => {
                let found = f.snapshot.runs(&filter);
                let stale = f.source == Source::StaleCache;
                if !found.is_empty() {
                    if stale {
                        eprintln!(
                            "警告: レート制限のため古いキャッシュ({}秒前)の結果です",
                            f.age.as_secs()
                        );
                    }
                    if f.snapshot.suites_truncated {
                        // 一致があれば 0 を返せる(一致が 0 件なら下で即エラーにする)ので、
                        // 取りこぼしは注記だけにする(JSON の形は変えない)。
                        eprintln!(
                            "注記: check suite が取得上限(50 件)を超えているため、一覧は不完全な可能性があります"
                        );
                    }
                    print_runs(&f, &found, a.json);
                    return Ok(exit::SUCCESS);
                }
                if stale {
                    // 古いキャッシュで「無い」とは断定できない。示された時間だけ待って再取得する。
                    let w = rate_wait(interval, f.retry_after);
                    if started.elapsed() + w >= wait {
                        return Err(VigiliaError::RateLimited {
                            retry_after: f.retry_after,
                            message: "古いキャッシュのまま --wait に達します".into(),
                        });
                    }
                    eprintln!("レート制限中: {}秒待機して再開します", w.as_secs());
                    std::thread::sleep(w);
                    continue;
                }
                // check suite が切り捨てられていて一致が 0 件なら、一致する run を取りこぼした可能性があり
                // 「無い」(7)とは断定できない。取りこぼした 51 件目以降は待っても取得できないため、
                // 期限まで待たず切り捨てを理由とするエラー(2)にする(`watch` と同じ)。
                if let Some(e) = truncated_no_match_error(&f.snapshot) {
                    return Err(e);
                }
                // 期限までの残りが最小間隔未満なら、もう取得せず「見つからない」で終える。
                let Some(pause) = next_poll_wait(started.elapsed(), wait, interval) else {
                    if a.json {
                        println!("[]");
                    }
                    eprintln!(
                        "条件に合う run は見つかりませんでした({} @ {})",
                        f.snapshot.repo,
                        render::short_sha(&f.snapshot.sha)
                    );
                    return Ok(exit::NOT_FOUND);
                };
                std::thread::sleep(pause);
            }
            Err(VigiliaError::RateLimited { retry_after, message }) => {
                let w = rate_wait(interval, retry_after);
                if started.elapsed() + w >= wait {
                    return Err(VigiliaError::RateLimited { retry_after, message });
                }
                eprintln!("レート制限({message}): {}秒待機して再開します", w.as_secs());
                std::thread::sleep(w);
            }
            Err(e) => return Err(e),
        }
    }
}

/// `runs` で一致が 0 件のとき、suite の切り捨てがあれば「見つからない」とは言えないためのエラー。
fn truncated_no_match_error(snap: &Snapshot) -> Option<VigiliaError> {
    snap.suites_truncated.then(|| {
        VigiliaError::Api(
            "check suite が取得上限(50 件)を超えており、条件に合う run が無いと断定できません。\
             GitHub の画面で確認してください"
                .into(),
        )
    })
}

fn print_runs(f: &Fetched, runs: &[RunInfo], json: bool) {
    if json {
        println!("{}", serde_json::to_string_pretty(runs).unwrap_or_default());
    } else {
        print!("{}", render::render_runs(&f.snapshot.repo, &f.snapshot.sha, runs));
        println!("{}", source_line(f));
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
                    let flt = filter_label(&a.target)
                        .map(|l| format!("({l})"))
                        .unwrap_or_default();
                    println!(
                        "{} @ {} を監視します{flt}",
                        f.snapshot.repo,
                        render::short_sha(&f.snapshot.sha)
                    );
                    header_printed = true;
                }
                print_changes(&f.snapshot, &mut seen);
                match watch_step(
                    &f,
                    is_filtered(&a.target),
                    started.elapsed(),
                    Duration::from_secs(a.wait_for_runs),
                )? {
                    WatchStep::Finish(overall) => {
                        print_final(&f.snapshot, overall);
                        return Ok(overall_code(overall));
                    }
                    WatchStep::NoRunsExpired => {
                        return Err(VigiliaError::NotFound(format!(
                            "{}秒待っても run が現れませんでした(ワークフローが無い/対象外、または絞り込み条件に合う run が無い可能性)",
                            a.wait_for_runs
                        )));
                    }
                    // レート制限で古いキャッシュを返された場合は、結論も未検出の期限も確定させず、
                    // 示された待ち時間の間 API を叩かずに待って再取得する。
                    WatchStep::StaleWait => {
                        let wait = rate_wait(interval, f.retry_after);
                        if started.elapsed() + wait >= timeout {
                            return Err(VigiliaError::RateLimited {
                                retry_after: f.retry_after,
                                message: "古いキャッシュのまま --timeout に達します".into(),
                            });
                        }
                        eprintln!("レート制限中: {}秒待機して再開します", wait.as_secs());
                        std::thread::sleep(wait);
                        continue;
                    }
                    WatchStep::Poll => {}
                }
            }
            Err(VigiliaError::RateLimited { retry_after, message }) => {
                // 落とさずに、示された時間(分からなければ interval)だけ待って再開する。
                let wait = rate_wait(interval, retry_after);
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

        // 期限までの残りが最小間隔未満なら、もう取得せずタイムアウトで終える。
        let Some(pause) = next_poll_wait(started.elapsed(), timeout, interval) else {
            println!("タイムアウト: {}秒経っても完了しませんでした", a.timeout);
            return Ok(exit::TIMEOUT);
        };
        std::thread::sleep(pause);
    }
}

/// `watch` の 1 回の取得結果に対する次の動作。
#[derive(Debug, PartialEq, Eq)]
enum WatchStep {
    /// 結論が確定した(終了コードへ)。
    Finish(Overall),
    /// run が現れないまま `--wait-for-runs` を超えた(未検出)。
    NoRunsExpired,
    /// 古いキャッシュしか得られていない。結論を確定させず、待ってから再取得する。
    StaleWait,
    /// 通常どおり次の取得まで待つ。
    Poll,
}

/// 取得結果から `watch` の次の動作を決める。古いキャッシュ(レート制限の代替)の間は、
/// 終端の結論も NoRuns の期限切れも確定させない。年齢の上限が無い古い断面で
/// 「完了」「未検出」と判断すると、現在の状態と食い違うため。
fn watch_step(
    f: &Fetched,
    filtered: bool,
    elapsed: Duration,
    wait_for_runs: Duration,
) -> Result<WatchStep> {
    if f.source == Source::StaleCache {
        return Ok(WatchStep::StaleWait);
    }
    let overall = f.snapshot.conclusive_overall(filtered)?;
    if overall.is_terminal() {
        return Ok(WatchStep::Finish(overall));
    }
    if overall == Overall::NoRuns && elapsed >= wait_for_runs {
        return Ok(WatchStep::NoRunsExpired);
    }
    Ok(WatchStep::Poll)
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
    fn レート制限の待ち時間はretry_afterを尊重しintervalを下回らない() {
        let i = Duration::from_secs(20);
        assert_eq!(rate_wait(i, Some(Duration::from_secs(300))), Duration::from_secs(300));
        assert_eq!(rate_wait(i, Some(Duration::from_secs(3))), i);
        assert_eq!(rate_wait(i, None), i);
    }

    use crate::model::Suite;

    fn fetched(source: Source, status: &str, conclusion: Option<&str>) -> Fetched {
        let suites = if status.is_empty() {
            vec![]
        } else {
            vec![Suite {
                app: "GitHub Actions".into(),
                run_id: Some(1),
                status: status.into(),
                conclusion: conclusion.map(String::from),
                ..Default::default()
            }]
        };
        Fetched {
            snapshot: Snapshot {
                repo: "o/r".into(),
                sha: "abc".into(),
                suites,
                truncated: false,
                suites_truncated: false,
                cost: None,
            },
            source,
            age: Duration::from_secs(900),
            retry_after: Some(Duration::from_secs(60)),
        }
    }

    #[test]
    fn statusは古いキャッシュなら断面を出しても結論とせず終了コード5() {
        // 古いキャッシュが「成功」「失敗」「run なし」でも結論にしない(5 = 結論を得られなかった)。
        for (st, c) in [("COMPLETED", Some("SUCCESS")), ("COMPLETED", Some("FAILURE")), ("", None)] {
            let (_, code) = status_verdict(&fetched(Source::StaleCache, st, c), false).unwrap();
            assert_eq!(code, exit::RATE_LIMITED, "{st} {c:?}");
        }
        // 古くなければ従来どおり(キャッシュ命中・API は結論の終了コード)。
        let (o, code) = status_verdict(&fetched(Source::Cache, "COMPLETED", Some("SUCCESS")), false).unwrap();
        assert_eq!((o, code), (Overall::Success, 0));
        let (_, code) = status_verdict(&fetched(Source::Api, "", None), false).unwrap();
        assert_eq!(code, exit::NOT_FOUND);
    }

    #[test]
    fn status_jsonは古いキャッシュならoverallをnullにし生の値をstale_overallへ() {
        let args = TargetArgs {
            target: None,
            repo: None,
            ttl: 15,
            no_cache: false,
            workflow: Some("ci.yml".into()),
            event: None,
        };
        let f = fetched(Source::StaleCache, "COMPLETED", Some("SUCCESS"));
        let v = status_json(&f, Overall::Success, &args);
        assert!(v["overall"].is_null(), "{v}");
        assert_eq!(v["stale_overall"], "success");
        assert_eq!(v["source"], "stale-cache");
        // 通常時は従来の形のまま(stale_overall を足さない)。
        let f = fetched(Source::Cache, "COMPLETED", Some("SUCCESS"));
        let v = status_json(&f, Overall::Success, &args);
        assert_eq!(v["overall"], "success");
        assert!(v.get("stale_overall").is_none(), "{v}");
        assert_eq!(v["source"], "cache");
    }

    #[test]
    fn runsは切り捨てがあれば一致0件を未検出とせずエラーにする() {
        let mut f = fetched(Source::Api, "COMPLETED", Some("SUCCESS"));
        assert!(truncated_no_match_error(&f.snapshot).is_none());
        f.snapshot.suites_truncated = true;
        match truncated_no_match_error(&f.snapshot) {
            Some(e @ VigiliaError::Api(_)) => assert_eq!(e.exit_code(), exit::ERROR),
            other => panic!("Api エラーのはず: {other:?}"),
        }
    }

    #[test]
    fn watchは古いキャッシュの間は終端も未検出も確定させない() {
        let w = Duration::from_secs(120);
        let late = Duration::from_secs(600);
        // 終端の結論(成功・失敗)でも、古いキャッシュなら待って再取得する。
        for c in ["SUCCESS", "FAILURE", "CANCELLED"] {
            let f = fetched(Source::StaleCache, "COMPLETED", Some(c));
            assert_eq!(watch_step(&f, false, late, w).unwrap(), WatchStep::StaleWait, "{c}");
        }
        // run なしの期限(--wait-for-runs)切れも、古いキャッシュでは確定させない。
        let f = fetched(Source::StaleCache, "", None);
        assert_eq!(watch_step(&f, false, late, w).unwrap(), WatchStep::StaleWait);
        // 古くなければ従来どおり確定する。
        let f = fetched(Source::Api, "COMPLETED", Some("SUCCESS"));
        assert_eq!(watch_step(&f, false, late, w).unwrap(), WatchStep::Finish(Overall::Success));
        let f = fetched(Source::Cache, "", None);
        assert_eq!(watch_step(&f, false, late, w).unwrap(), WatchStep::NoRunsExpired);
        assert_eq!(watch_step(&f, false, Duration::from_secs(5), w).unwrap(), WatchStep::Poll);
        let f = fetched(Source::Api, "IN_PROGRESS", None);
        assert_eq!(watch_step(&f, false, late, w).unwrap(), WatchStep::Poll);
    }

    #[test]
    fn 期限までの残りが最小間隔未満なら追加取得しない() {
        let i = Duration::from_secs(20);
        let lim = Duration::from_secs(100);
        // 残りが十分あれば interval(残りが短ければ残りまで)待つ。
        assert_eq!(next_poll_wait(Duration::from_secs(10), lim, i), Some(i));
        assert_eq!(
            next_poll_wait(Duration::from_secs(85), lim, i),
            Some(Duration::from_secs(15))
        );
        // 残りちょうど 5 秒は取得してよい(取得間隔が 5 秒以上になる)。
        assert_eq!(
            next_poll_wait(Duration::from_secs(95), lim, i),
            Some(Duration::from_secs(5))
        );
        // 残りが 5 秒未満・期限超過なら取得しない(5 秒未満の間隔で API を叩かない)。
        assert_eq!(next_poll_wait(Duration::from_millis(95_001), lim, i), None);
        assert_eq!(next_poll_wait(Duration::from_secs(100), lim, i), None);
        assert_eq!(next_poll_wait(Duration::from_secs(500), lim, i), None);
        // --wait 0(1 回だけ取得)でも追加取得しない。
        assert_eq!(next_poll_wait(Duration::ZERO, Duration::ZERO, i), None);
    }

    #[test]
    fn budget行の整形() {
        let r = Resource { limit: 5000, remaining: 0, used: 5000, reset: 1_000_090 };
        let line = budget_line("REST (core)", &r, 1_000_000);
        assert!(line.contains("残り     0/5000"), "{line}");
        assert!(line.contains("あと 1m30s"), "{line}");
    }
}
