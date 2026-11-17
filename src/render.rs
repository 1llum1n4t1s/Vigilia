//! 人間向けの簡潔なテキスト整形。
//!
//! 標準出力は AI エージェントも読むため、1 job = 1 行、装飾なしを基本にする。

use crate::model::{Job, Overall, RunInfo, Snapshot, Suite};
use chrono::{DateTime, Local, Utc};

/// status / conclusion を日本語の短いラベルにする。
pub fn state_label(status: &str, conclusion: Option<&str>) -> &'static str {
    if status != "COMPLETED" {
        return match status {
            "IN_PROGRESS" => "実行中",
            _ => "待機中", // QUEUED / WAITING / PENDING / REQUESTED
        };
    }
    match conclusion.unwrap_or("") {
        "SUCCESS" => "成功",
        "FAILURE" => "失敗",
        "CANCELLED" => "取消",
        "SKIPPED" => "スキップ",
        "NEUTRAL" => "中立",
        "TIMED_OUT" => "タイムアウト",
        "ACTION_REQUIRED" => "要対応",
        "STARTUP_FAILURE" => "起動失敗",
        "STALE" => "失効",
        _ => "完了",
    }
}

pub fn job_label(job: &Job) -> &'static str {
    state_label(&job.status, job.conclusion.as_deref())
}

pub fn suite_label(suite: &Suite) -> &'static str {
    state_label(&suite.status, suite.conclusion.as_deref())
}

/// suite の表示名(例: `CI #7 (push)`)。
pub fn suite_title(suite: &Suite) -> String {
    let mut s = suite
        .workflow
        .clone()
        .unwrap_or_else(|| suite.app.clone());
    if let Some(n) = suite.run_number {
        s.push_str(&format!(" #{n}"));
    }
    if let Some(e) = &suite.event {
        s.push_str(&format!(" ({e})"));
    }
    s
}

/// 秒数を `1m05s` のような短い表記へ。
pub fn fmt_secs(secs: i64) -> String {
    let secs = secs.max(0);
    if secs >= 3600 {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

/// job の所要時間(完了済みは開始〜完了、実行中は開始〜現在)。
pub fn job_elapsed(job: &Job, now: DateTime<Utc>) -> Option<String> {
    let start = DateTime::parse_from_rfc3339(job.started_at.as_deref()?).ok()?;
    let end = match job.completed_at.as_deref().and_then(|c| DateTime::parse_from_rfc3339(c).ok()) {
        Some(e) => e.with_timezone(&Utc),
        None => now,
    };
    Some(fmt_secs((end - start.with_timezone(&Utc)).num_seconds()))
}

/// 見出しの結論表記。古いキャッシュ(`stale`)の断面は現在の結論ではないため、
/// 「結論: 成功」とは読めない表記にする(終了コード 5 と食い違わせない)。
pub fn conclusion_text(overall: Overall, stale: bool) -> String {
    if stale {
        format!("未確定(レート制限中の古い断面: {})", overall.label())
    } else {
        overall.label().to_string()
    }
}

/// 完全な状態表示(`status` 用)。`stale` は古いキャッシュの断面であること。
pub fn render_snapshot(snap: &Snapshot, now: DateTime<Utc>, stale: bool) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{} @ {}  結論: {}\n",
        snap.repo,
        short_sha(&snap.sha),
        conclusion_text(snap.overall(), stale)
    ));
    if snap.suites.is_empty() {
        out.push_str("  (run / check suite はまだありません)\n");
    }
    for suite in &snap.suites {
        out.push_str(&format!("  {} [{}]\n", suite_title(suite), suite_label(suite)));
        for job in &suite.jobs {
            let elapsed = job_elapsed(job, now).map(|e| format!("  {e}")).unwrap_or_default();
            out.push_str(&format!("    {:<8} {}{}\n", job_label(job), job.name, elapsed));
        }
        if let Some(url) = &suite.url {
            out.push_str(&format!("    {url}\n"));
        }
    }
    if snap.truncated || snap.suites_truncated {
        out.push_str("  注意: suite / job が多いため一部を省略しています\n");
    }
    out
}

/// `runs` の人間向け表示。1 run = 1 行(列: run id / 状態 / workflow / event / 作成時刻 / ブランチ / URL)。
pub fn render_runs(repo: &str, sha: &str, runs: &[RunInfo]) -> String {
    let mut out = format!("{repo} @ {}  run {} 件\n", short_sha(sha), runs.len());
    let dash = |o: &Option<String>| o.clone().unwrap_or_else(|| "-".into());
    for r in runs {
        let label = state_label(
            &r.status.to_ascii_uppercase(),
            r.conclusion.as_deref().map(str::to_ascii_uppercase).as_deref(),
        );
        out.push_str(&format!(
            "  {}  {:<8}  {} [{}]  {}  {}  {}  {}\n",
            r.run_id,
            label,
            dash(&r.workflow),
            dash(&r.workflow_path),
            dash(&r.event),
            dash(&r.created_at),
            dash(&r.branch),
            dash(&r.url),
        ));
    }
    out
}

/// 失敗 job の一覧(`watch` の最終出力用)。
pub fn failed_jobs(snap: &Snapshot) -> Vec<String> {
    let mut v = Vec::new();
    for s in &snap.suites {
        for j in &s.jobs {
            if j.status == "COMPLETED"
                && matches!(
                    j.conclusion.as_deref(),
                    Some("FAILURE" | "TIMED_OUT" | "STARTUP_FAILURE" | "ACTION_REQUIRED")
                )
            {
                v.push(format!(
                    "{} / {} [{}] {}",
                    suite_title(s),
                    j.name,
                    job_label(j),
                    j.url.clone().unwrap_or_default()
                ));
            }
        }
    }
    v
}

pub fn short_sha(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

/// epoch 秒をローカル時刻 + UTC 表記にする(例: `2026-11-16 21:51 +09:00 (12:51 UTC)`。`%Z` はローカルのオフセット表記になる)。
pub fn fmt_reset(epoch: i64) -> String {
    match DateTime::<Utc>::from_timestamp(epoch, 0) {
        Some(utc) => {
            let local = utc.with_timezone(&Local);
            format!("{} ({} UTC)", local.format("%Y-%m-%d %H:%M %Z"), utc.format("%H:%M"))
        }
        None => "?".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ラベル() {
        assert_eq!(state_label("IN_PROGRESS", None), "実行中");
        assert_eq!(state_label("QUEUED", None), "待機中");
        assert_eq!(state_label("COMPLETED", Some("SUCCESS")), "成功");
        assert_eq!(state_label("COMPLETED", Some("TIMED_OUT")), "タイムアウト");
    }

    #[test]
    fn 秒の整形() {
        assert_eq!(fmt_secs(5), "5s");
        assert_eq!(fmt_secs(65), "1m05s");
        assert_eq!(fmt_secs(3700), "1h01m");
        assert_eq!(fmt_secs(-3), "0s");
    }

    #[test]
    fn 古い断面の見出しは結論と読めない表記になる() {
        let snap = Snapshot {
            repo: "o/r".into(),
            sha: "abcdef0123".into(),
            suites: vec![Suite {
                run_id: Some(1),
                status: "COMPLETED".into(),
                conclusion: Some("SUCCESS".into()),
                ..Default::default()
            }],
            truncated: false,
            suites_truncated: false,
            cost: None,
        };
        let now = Utc::now();
        let fresh = render_snapshot(&snap, now, false);
        assert!(fresh.starts_with("o/r @ abcdef0  結論: 成功\n"), "{fresh}");
        let stale = render_snapshot(&snap, now, true);
        assert!(
            stale.starts_with("o/r @ abcdef0  結論: 未確定(レート制限中の古い断面: 成功)\n"),
            "{stale}"
        );
        assert!(!stale.contains("結論: 成功"), "{stale}");
    }

    #[test]
    fn 短縮sha() {
        assert_eq!(short_sha("e8c8bb3a2eb9"), "e8c8bb3");
        assert_eq!(short_sha("abc"), "abc");
    }

    #[test]
    fn run一覧の整形() {
        let r = RunInfo {
            run_id: 5,
            run_number: Some(1),
            run_attempt: Some(1),
            workflow: Some("Release".into()),
            workflow_path: None,
            event: Some("push".into()),
            status: "completed".into(),
            conclusion: Some("failure".into()),
            created_at: Some("2026-11-17T18:17:24Z".into()),
            url: None,
            head_sha: "abcdef0123".into(),
            branch: None,
        };
        let out = render_runs("o/r", "abcdef0123", &[r]);
        assert!(out.starts_with("o/r @ abcdef0  run 1 件\n"), "{out}");
        assert!(out.contains("5  失敗"), "{out}");
        assert!(out.contains("Release [-]  push  2026-11-17T18:17:24Z  -  -"), "{out}");
    }
}
