//! CI 状態のドメインモデルと、GraphQL レスポンスからの変換。
//!
//! 設計意図:
//! - GraphQL の生レスポンス型(`Raw*`)と、キャッシュ/JSON 出力に使う
//!   正規化済みの `Snapshot` を分離する。キャッシュ形式を API 都合から切り離すため。
//! - 「結論(Overall)」の判定はここに集約し、`watch` と `status` で同じ規則を使う。

use crate::error::{Result, VigiliaError};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// 1 回の取得で得られた CI 状態の断面。キャッシュにそのまま保存する。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// 実際のリポジトリ(`OWNER/REPO`)。
    pub repo: String,
    /// 対象コミットの完全な SHA。
    pub sha: String,
    /// 関連する check suite(ワークフロー run 単位)。
    pub suites: Vec<Suite>,
    /// check run(job)が上限を超えて切り捨てられた。表示上の注意のみ(suite 自体の結論は GitHub が集計済み)。
    pub truncated: bool,
    /// check suite が上限を超えて切り捨てられた。結論(特に「成功」)を保証できない。
    #[serde(default)]
    pub suites_truncated: bool,
    /// この取得で消費した GraphQL コストと残り枠(取得できた場合)。
    pub cost: Option<CostInfo>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Suite {
    pub app: String,
    pub workflow: Option<String>,
    /// workflow ファイルのパス(例: `.github/workflows/release.yml`)。取得できなければ None。
    #[serde(default)]
    pub workflow_path: Option<String>,
    pub run_id: Option<u64>,
    pub run_number: Option<u64>,
    /// 再実行の通番(初回は 1)。
    #[serde(default)]
    pub run_attempt: Option<u64>,
    pub event: Option<String>,
    /// run の作成時刻(RFC 3339、UTC)。
    #[serde(default)]
    pub created_at: Option<String>,
    /// run を起こしたブランチ名(タグ起点などでは None)。
    #[serde(default)]
    pub branch: Option<String>,
    pub url: Option<String>,
    /// GraphQL の CheckStatusState(大文字: QUEUED / IN_PROGRESS / COMPLETED ...)。
    pub status: String,
    /// GraphQL の CheckConclusionState(大文字)。未完了なら None。
    pub conclusion: Option<String>,
    pub jobs: Vec<Job>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub name: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub url: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CostInfo {
    pub cost: u64,
    pub remaining: u64,
    pub reset_at: String,
}

/// 対象全体の結論。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Overall {
    /// run / check suite がまだ 1 つも無い(push 直後など)。
    NoRuns,
    InProgress,
    Success,
    Failure,
    Cancelled,
}

impl Overall {
    /// 完了(これ以上状態が変わらない)か。
    pub fn is_terminal(self) -> bool {
        matches!(self, Overall::Success | Overall::Failure | Overall::Cancelled)
    }

    pub fn label(self) -> &'static str {
        match self {
            Overall::NoRuns => "run なし",
            Overall::InProgress => "実行中",
            Overall::Success => "成功",
            Overall::Failure => "失敗",
            Overall::Cancelled => "取消",
        }
    }
}

/// 失敗扱いにする conclusion。
fn is_failure_conclusion(c: &str) -> bool {
    matches!(
        c,
        "FAILURE" | "TIMED_OUT" | "STARTUP_FAILURE" | "ACTION_REQUIRED"
    )
}

fn is_cancel_conclusion(c: &str) -> bool {
    matches!(c, "CANCELLED" | "STALE")
}

impl Snapshot {
    /// 結論を返す。ただし check suite が切り捨てられた状態で「成功」とは断定できないため、
    /// その場合はエラーにする(取りこぼした suite が失敗している可能性がある)。
    pub fn conclusive_overall(&self) -> Result<Overall> {
        let overall = self.overall();
        if self.suites_truncated && overall == Overall::Success {
            return Err(VigiliaError::Api(
                "check suite が取得上限(50 件)を超えており、成功と断定できません。\
                 GitHub の画面で確認してください"
                    .into(),
            ));
        }
        Ok(overall)
    }

    /// 全 suite を畳み込んだ結論。
    /// 未完了が 1 つでもあれば実行中、全完了なら 失敗 > 取消 > 成功 の優先順。
    pub fn overall(&self) -> Overall {
        if self.suites.is_empty() {
            return Overall::NoRuns;
        }
        if self.suites.iter().any(|s| s.status != "COMPLETED") {
            return Overall::InProgress;
        }
        let concl = |s: &Suite| s.conclusion.clone().unwrap_or_default();
        if self.suites.iter().any(|s| is_failure_conclusion(&concl(s))) {
            Overall::Failure
        } else if self.suites.iter().any(|s| is_cancel_conclusion(&concl(s))) {
            Overall::Cancelled
        } else {
            Overall::Success
        }
    }
}

// ---------------------------------------------------------------------------
// workflow / event による絞り込みと run 一覧
// ---------------------------------------------------------------------------

/// run の絞り込み条件。`watch` / `status` は workflow と event、`runs` は since も使う。
///
/// 絞り込みは取得(GraphQL・キャッシュ)の後に `Snapshot` へ適用する。したがってキャッシュは
/// 条件に依存せず、条件違いのセッション同士でも安全に共有できる。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunFilter {
    /// workflow の表示名、またはファイルパス(`.github/workflows/x.yml` / `x.yml`)。
    pub workflow: Option<String>,
    /// イベント名(`push` / `workflow_dispatch` など。大文字小文字は区別しない)。
    pub event: Option<String>,
    /// この時刻以後に作成された run だけ(`runs` 専用)。
    pub since: Option<DateTime<Utc>>,
}

/// パスの末尾(ファイル名)。
fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

impl RunFilter {
    /// workflow / event のどちらも指定されていない(since は含めない)。
    pub fn is_unfiltered(&self) -> bool {
        self.workflow.is_none() && self.event.is_none()
    }

    /// workflow の一致規則: 指定値が、表示名・ファイルパス全体・ファイル名のいずれかと完全一致する。
    /// 大文字小文字は区別する(GitHub のファイル名・表示名の扱いに合わせる)。
    fn workflow_matches(&self, suite: &Suite) -> bool {
        let Some(spec) = self.workflow.as_deref() else {
            return true;
        };
        suite.workflow.as_deref() == Some(spec)
            || suite.workflow_path.as_deref() == Some(spec)
            || suite.workflow_path.as_deref().map(file_name) == Some(spec)
    }

    /// 1 つの suite(= workflow run)が条件に合うか。workflow run を持たない suite は常に不一致。
    pub fn matches(&self, suite: &Suite) -> bool {
        if suite.run_id.is_none() || !self.workflow_matches(suite) {
            return false;
        }
        if let Some(ev) = &self.event
            && !suite
                .event
                .as_deref()
                .is_some_and(|e| e.eq_ignore_ascii_case(ev))
        {
            return false;
        }
        if let Some(since) = self.since {
            let created = suite
                .created_at
                .as_deref()
                .and_then(|c| DateTime::parse_from_rfc3339(c).ok());
            // 作成時刻が分からない run は「以後」と断定できないため除外する。
            if !created.is_some_and(|c| c.with_timezone(&Utc) >= since) {
                return false;
            }
        }
        true
    }
}

/// workflow を一意に見分けるキー(パス優先、無ければ表示名)。
fn workflow_identity(s: &Suite) -> String {
    s.workflow_path
        .clone()
        .or_else(|| s.workflow.clone())
        .unwrap_or_default()
}

/// 「どちらが新しい run か」の比較キー(作成時刻 → run 番号 → run id)。
fn recency(s: &Suite) -> (String, u64, u64) {
    (
        s.created_at.clone().unwrap_or_default(),
        s.run_number.unwrap_or(0),
        s.run_id.unwrap_or(0),
    )
}

impl Snapshot {
    /// workflow / event で絞った `Snapshot` を返す(`watch` / `status` の `--workflow` / `--event`)。
    ///
    /// 複数一致時の規則:
    /// 1. 指定した workflow が複数の別 workflow に一致する(例: 表示名 `CI` を持つ 2 つのファイル)
    ///    場合は、どれを指すか決められないため使用エラーにする。
    /// 2. 同じ workflow・同じ event の run が複数ある(再実行で新しい run が作られた、
    ///    同じコミットへ同じ push が重なった等)場合は、最も新しい run だけを結論に使う。
    ///    古い run は新しい run に取って代わられたものとして扱う。
    /// 3. event が異なる run(push と workflow_dispatch など)は別物として全て結論に含める。
    ///    片方だけを見たいときは `--event` を併用する。
    ///
    /// 一致する run が無ければ suite は空になり、結論は `NoRuns`(待機 / 未検出)になる。
    pub fn filtered(&self, filter: &RunFilter) -> Result<Snapshot> {
        let matched: Vec<&Suite> = self.suites.iter().filter(|s| filter.matches(s)).collect();
        if let Some(spec) = &filter.workflow {
            let mut ids: Vec<String> = matched.iter().map(|s| workflow_identity(s)).collect();
            ids.sort();
            ids.dedup();
            if ids.len() > 1 {
                return Err(VigiliaError::Usage(format!(
                    "--workflow '{spec}' が複数の workflow に一致します: {}。\
                     ファイルパス(例: .github/workflows/ci.yml)で指定してください",
                    ids.join(", ")
                )));
            }
        }
        // (workflow, event) ごとに最新の 1 件だけを残す。元の並びは保つ。
        let mut latest: std::collections::HashMap<(String, String), (String, u64, u64)> =
            std::collections::HashMap::new();
        for s in &matched {
            let key = (workflow_identity(s), s.event.clone().unwrap_or_default());
            let r = recency(s);
            latest
                .entry(key)
                .and_modify(|cur| {
                    if r > *cur {
                        *cur = r.clone();
                    }
                })
                .or_insert(r);
        }
        let suites: Vec<Suite> = matched
            .into_iter()
            .filter(|s| {
                let key = (workflow_identity(s), s.event.clone().unwrap_or_default());
                latest.get(&key) == Some(&recency(s))
            })
            .cloned()
            .collect();
        Ok(Snapshot {
            suites,
            ..self.clone()
        })
    }
}

/// `runs` が出力する 1 件の workflow run。`--json` の要素そのもの。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunInfo {
    /// run id(REST の `databaseId` と同じ)。
    pub run_id: u64,
    pub run_number: Option<u64>,
    pub run_attempt: Option<u64>,
    /// workflow の表示名。
    pub workflow: Option<String>,
    /// workflow ファイルのパス。
    pub workflow_path: Option<String>,
    pub event: Option<String>,
    /// 小文字の状態(`queued` / `in_progress` / `completed` など。`gh run list` と同じ表記)。
    pub status: String,
    /// 小文字の結論(`success` / `failure` など)。未完了なら null。
    pub conclusion: Option<String>,
    pub created_at: Option<String>,
    pub url: Option<String>,
    pub head_sha: String,
    pub branch: Option<String>,
}

impl Snapshot {
    /// 条件に合う run の一覧。作成時刻の新しい順(同時刻は run id の大きい順)で決定的に並べる。
    /// workflow が複数に一致しても全て返す(絞り込みの曖昧さをエラーにするのは結論を出す側だけ)。
    pub fn runs(&self, filter: &RunFilter) -> Vec<RunInfo> {
        let mut v: Vec<RunInfo> = self
            .suites
            .iter()
            .filter(|s| filter.matches(s))
            .filter_map(|s| {
                Some(RunInfo {
                    run_id: s.run_id?,
                    run_number: s.run_number,
                    run_attempt: s.run_attempt,
                    workflow: s.workflow.clone(),
                    workflow_path: s.workflow_path.clone(),
                    event: s.event.clone(),
                    status: s.status.to_ascii_lowercase(),
                    conclusion: s.conclusion.as_ref().map(|c| c.to_ascii_lowercase()),
                    created_at: s.created_at.clone(),
                    url: s.url.clone(),
                    head_sha: self.sha.clone(),
                    branch: s.branch.clone(),
                })
            })
            .collect();
        v.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then(b.run_id.cmp(&a.run_id))
        });
        v
    }
}

// ---------------------------------------------------------------------------
// GraphQL 生レスポンス
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct GqlResponse {
    data: Option<GqlData>,
    errors: Option<Vec<GqlError>>,
}

#[derive(Debug, Deserialize)]
pub struct GqlError {
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub message: String,
}

#[derive(Debug, Deserialize)]
struct GqlData {
    repository: Option<RawRepository>,
    node: Option<RawNode>,
    #[serde(rename = "rateLimit")]
    rate_limit: Option<RawRateLimit>,
}

#[derive(Debug, Deserialize)]
struct RawRateLimit {
    cost: u64,
    remaining: u64,
    #[serde(rename = "resetAt")]
    reset_at: String,
}

#[derive(Debug, Deserialize)]
struct RawRepository {
    #[serde(rename = "nameWithOwner")]
    name_with_owner: String,
    object: Option<RawCommit>,
}

#[derive(Debug, Deserialize)]
struct RawCommit {
    oid: Option<String>,
    #[serde(rename = "checkSuites")]
    check_suites: Option<RawSuiteConn>,
}

/// `node(id:)` で引いた WorkflowRun。
#[derive(Debug, Deserialize)]
struct RawNode {
    #[serde(rename = "checkSuite")]
    check_suite: Option<RawNodeSuite>,
}

#[derive(Debug, Deserialize)]
struct RawNodeSuite {
    repository: RawRepoName,
    commit: RawOid,
    #[serde(flatten)]
    suite: RawSuite,
}

#[derive(Debug, Deserialize)]
struct RawRepoName {
    #[serde(rename = "nameWithOwner")]
    name_with_owner: String,
}

#[derive(Debug, Deserialize)]
struct RawOid {
    oid: String,
}

#[derive(Debug, Deserialize)]
struct RawSuiteConn {
    #[serde(rename = "pageInfo")]
    page_info: Option<RawPageInfo>,
    nodes: Option<Vec<Option<RawSuite>>>,
}

#[derive(Debug, Deserialize)]
struct RawPageInfo {
    #[serde(rename = "hasNextPage")]
    has_next_page: bool,
}

#[derive(Debug, Deserialize)]
struct RawSuite {
    app: Option<RawApp>,
    status: String,
    conclusion: Option<String>,
    #[serde(rename = "workflowRun")]
    workflow_run: Option<RawRun>,
    #[serde(rename = "checkRuns")]
    check_runs: Option<RawRunConn>,
    branch: Option<RawBranch>,
}

#[derive(Debug, Deserialize)]
struct RawApp {
    name: String,
}

#[derive(Debug, Deserialize)]
struct RawBranch {
    name: String,
}

#[derive(Debug, Deserialize)]
struct RawRunFile {
    path: String,
}

#[derive(Debug, Deserialize)]
struct RawRun {
    #[serde(rename = "databaseId")]
    database_id: Option<u64>,
    #[serde(rename = "runNumber")]
    run_number: Option<u64>,
    event: Option<String>,
    url: Option<String>,
    workflow: Option<RawWorkflow>,
    #[serde(rename = "createdAt")]
    created_at: Option<String>,
    #[serde(rename = "runAttempt")]
    run_attempt: Option<u64>,
    file: Option<RawRunFile>,
}

#[derive(Debug, Deserialize)]
struct RawWorkflow {
    name: String,
}

#[derive(Debug, Deserialize)]
struct RawRunConn {
    #[serde(rename = "pageInfo")]
    page_info: Option<RawPageInfo>,
    nodes: Option<Vec<Option<RawCheckRun>>>,
}

#[derive(Debug, Deserialize)]
struct RawCheckRun {
    name: String,
    status: String,
    conclusion: Option<String>,
    #[serde(rename = "detailsUrl")]
    details_url: Option<String>,
    #[serde(rename = "startedAt")]
    started_at: Option<String>,
    #[serde(rename = "completedAt")]
    completed_at: Option<String>,
}

/// check suite を `Suite` へ正規化する。第 2 戻り値は check run の切り捨て有無。
fn convert_suite(raw: RawSuite) -> (Suite, bool) {
    let mut truncated = false;
    let mut jobs = Vec::new();
    if let Some(conn) = raw.check_runs {
        if conn.page_info.map(|p| p.has_next_page).unwrap_or(false) {
            truncated = true;
        }
        for r in conn.nodes.unwrap_or_default().into_iter().flatten() {
            jobs.push(Job {
                name: r.name,
                status: r.status,
                conclusion: r.conclusion,
                url: r.details_url,
                started_at: r.started_at,
                completed_at: r.completed_at,
            });
        }
    }
    let run = raw.workflow_run;
    let suite = Suite {
        app: raw.app.map(|a| a.name).unwrap_or_else(|| "?".into()),
        workflow: run
            .as_ref()
            .and_then(|r| r.workflow.as_ref().map(|w| w.name.clone())),
        workflow_path: run
            .as_ref()
            .and_then(|r| r.file.as_ref().map(|f| f.path.clone())),
        run_id: run.as_ref().and_then(|r| r.database_id),
        run_number: run.as_ref().and_then(|r| r.run_number),
        run_attempt: run.as_ref().and_then(|r| r.run_attempt),
        created_at: run.as_ref().and_then(|r| r.created_at.clone()),
        branch: raw.branch.map(|b| b.name),
        event: run.as_ref().and_then(|r| r.event.clone()),
        url: run.as_ref().and_then(|r| r.url.clone()),
        status: raw.status,
        conclusion: raw.conclusion,
        jobs,
    };
    (suite, truncated)
}

/// GraphQL レスポンス(JSON 値)の `errors` を評価してエラーへ変換する。
/// `data` が得られている部分的エラーは呼び出し側で扱うため、ここでは
/// 「致命的(data が無い)」ときと「レート制限」だけを返す。
pub fn check_gql_errors(value: &serde_json::Value) -> Result<()> {
    let errors: Vec<GqlError> = match value.get("errors") {
        Some(e) => serde_json::from_value(e.clone()).unwrap_or_default(),
        None => return Ok(()),
    };
    if errors.is_empty() {
        return Ok(());
    }
    if errors
        .iter()
        .any(|e| e.kind.as_deref() == Some("RATE_LIMITED"))
    {
        return Err(VigiliaError::RateLimited {
            retry_after: None,
            message: errors
                .iter()
                .map(|e| e.message.as_str())
                .collect::<Vec<_>>()
                .join("; "),
        });
    }
    let has_data = value
        .get("data")
        .map(|d| !d.is_null())
        .unwrap_or(false);
    let msg = errors
        .iter()
        .map(|e| e.message.as_str())
        .collect::<Vec<_>>()
        .join("; ");
    // NOT_FOUND は repository / node が null になるだけで data 自体は返るため、
    // 後段(parse_*)で対象が欠けていることを判定する。ここでは data 無しだけ致命扱い。
    if errors.iter().any(|e| e.kind.as_deref() == Some("NOT_FOUND")) && has_data {
        return Ok(());
    }
    if errors.iter().any(|e| e.kind.as_deref() == Some("NOT_FOUND")) {
        return Err(VigiliaError::NotFound(msg));
    }
    Err(VigiliaError::Api(msg))
}

fn parse_response(value: &serde_json::Value) -> Result<GqlData> {
    check_gql_errors(value)?;
    let resp: GqlResponse = serde_json::from_value(value.clone())
        .map_err(|e| VigiliaError::Api(format!("レスポンスを解釈できません: {e}")))?;
    let _ = resp.errors;
    resp.data
        .ok_or_else(|| VigiliaError::Api("レスポンスに data がありません".into()))
}

fn cost_of(rl: Option<RawRateLimit>) -> Option<CostInfo> {
    rl.map(|r| CostInfo {
        cost: r.cost,
        remaining: r.remaining,
        reset_at: r.reset_at,
    })
}

/// 「関連する suite」だけを残す。
/// GitHub Actions 以外のアプリ(例: 常に QUEUED のまま残る check suite)が
/// check run を持たない場合、永遠に完了しないため監視対象から除外する。
fn keep_relevant(suite: &Suite) -> bool {
    suite.run_id.is_some() || !suite.jobs.is_empty()
}

/// sha 起点のクエリ(`repository.object`)のレスポンスを `Snapshot` へ変換する。
pub fn parse_commit_response(value: &serde_json::Value, target: &str) -> Result<Snapshot> {
    let data = parse_response(value)?;
    let repo = data.repository.ok_or_else(|| {
        VigiliaError::NotFound(
            "リポジトリが見つかりません(存在しないか、トークンに権限がありません)".into(),
        )
    })?;
    let commit = repo.object.ok_or_else(|| {
        VigiliaError::NotFound(format!("コミット {target} が見つかりません"))
    })?;
    let oid = commit.oid.ok_or_else(|| {
        VigiliaError::NotFound(format!("{target} はコミットを指していません"))
    })?;
    let mut truncated = false;
    let mut suites_truncated = false;
    let mut suites = Vec::new();
    if let Some(conn) = commit.check_suites {
        suites_truncated = conn.page_info.map(|p| p.has_next_page).unwrap_or(false);
        for raw in conn.nodes.unwrap_or_default().into_iter().flatten() {
            let (s, t) = convert_suite(raw);
            truncated |= t;
            if keep_relevant(&s) {
                suites.push(s);
            }
        }
    }
    Ok(Snapshot {
        repo: repo.name_with_owner,
        sha: oid,
        suites,
        truncated,
        suites_truncated,
        cost: cost_of(data.rate_limit),
    })
}

/// run id 起点のクエリ(`node`)のレスポンスを `Snapshot` へ変換する。
pub fn parse_run_response(value: &serde_json::Value, run_id: u64) -> Result<Snapshot> {
    let data = parse_response(value)?;
    let node = data
        .node
        .ok_or_else(|| VigiliaError::NotFound(format!("run {run_id} が見つかりません")))?;
    let cs = node.check_suite.ok_or_else(|| {
        VigiliaError::NotFound(format!("ID {run_id} は workflow run ではありません"))
    })?;
    let (suite, truncated) = convert_suite(cs.suite);
    Ok(Snapshot {
        repo: cs.repository.name_with_owner,
        sha: cs.commit.oid,
        suites: vec![suite],
        truncated,
        suites_truncated: false,
        cost: cost_of(data.rate_limit),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn commit_fixture() -> serde_json::Value {
        json!({
          "data": {
            "repository": {
              "nameWithOwner": "octo/demo",
              "object": {
                "oid": "e8c8bb3a2eb9372022b4d7c6ed96e8913f442cd8",
                "checkSuites": {
                  "pageInfo": {"hasNextPage": false},
                  "nodes": [
                    { "app": {"name": "Claude"}, "status": "QUEUED", "conclusion": null,
                      "workflowRun": null,
                      "checkRuns": {"pageInfo":{"hasNextPage":false},"nodes":[]} },
                    { "app": {"name": "GitHub Actions"}, "status": "IN_PROGRESS", "conclusion": null,
                      "workflowRun": {"databaseId": 100, "runNumber": 7, "event": "push",
                                      "url": "https://github.com/octo/demo/actions/runs/100",
                                      "workflow": {"name": "CI"}},
                      "checkRuns": {"pageInfo":{"hasNextPage":false}, "nodes": [
                        {"name":"build","status":"COMPLETED","conclusion":"SUCCESS",
                         "detailsUrl":"https://example/job/1",
                         "startedAt":"2026-11-16T00:00:00Z","completedAt":"2026-11-16T00:01:00Z"},
                        {"name":"test","status":"IN_PROGRESS","conclusion":null,
                         "detailsUrl":null,"startedAt":"2026-11-16T00:01:00Z","completedAt":null}
                      ]} }
                  ]
                }
              }
            },
            "rateLimit": {"cost": 1, "remaining": 4990, "resetAt": "2026-11-16T13:35:43Z"}
          }
        })
    }

    #[test]
    fn コミット応答を正規化し無関係なsuiteを除外する() {
        let snap = parse_commit_response(&commit_fixture(), "e8c8bb3").unwrap();
        assert_eq!(snap.repo, "octo/demo");
        assert_eq!(snap.sha, "e8c8bb3a2eb9372022b4d7c6ed96e8913f442cd8");
        // Claude アプリの QUEUED suite(run も job も無い)は除外される。
        assert_eq!(snap.suites.len(), 1);
        assert_eq!(snap.suites[0].workflow.as_deref(), Some("CI"));
        assert_eq!(snap.suites[0].jobs.len(), 2);
        assert_eq!(snap.overall(), Overall::InProgress);
        assert_eq!(snap.cost.as_ref().unwrap().remaining, 4990);
        assert!(!snap.truncated);
    }

    #[test]
    fn 全完了時の結論は失敗_取消_成功の優先順() {
        let mut snap = parse_commit_response(&commit_fixture(), "x").unwrap();
        snap.suites[0].status = "COMPLETED".into();
        snap.suites[0].conclusion = Some("SUCCESS".into());
        assert_eq!(snap.overall(), Overall::Success);
        let mut other = snap.suites[0].clone();
        other.conclusion = Some("CANCELLED".into());
        snap.suites.push(other.clone());
        assert_eq!(snap.overall(), Overall::Cancelled);
        other.conclusion = Some("TIMED_OUT".into());
        snap.suites.push(other);
        assert_eq!(snap.overall(), Overall::Failure);
        // skipped / neutral は成功扱い。
        snap.suites.truncate(1);
        snap.suites[0].conclusion = Some("SKIPPED".into());
        assert_eq!(snap.overall(), Overall::Success);
    }

    #[test]
    fn suite切り捨て時は成功と断定しない() {
        let mut snap = parse_commit_response(&commit_fixture(), "x").unwrap();
        snap.suites[0].status = "COMPLETED".into();
        snap.suites[0].conclusion = Some("SUCCESS".into());
        snap.suites_truncated = true;
        assert!(matches!(snap.conclusive_overall(), Err(VigiliaError::Api(_))));
        // 失敗は切り捨てがあっても確定できる。
        snap.suites[0].conclusion = Some("FAILURE".into());
        assert_eq!(snap.conclusive_overall().unwrap(), Overall::Failure);
        // pageInfo の hasNextPage から検知される。
        let mut v = commit_fixture();
        v["data"]["repository"]["object"]["checkSuites"]["pageInfo"]["hasNextPage"] =
            serde_json::json!(true);
        assert!(parse_commit_response(&v, "x").unwrap().suites_truncated);
    }

    #[test]
    fn suiteが無ければrunなし() {
        let v = json!({"data":{"repository":{"nameWithOwner":"a/b","object":
            {"oid":"abc","checkSuites":{"nodes":[]}}}}});
        let snap = parse_commit_response(&v, "abc").unwrap();
        assert_eq!(snap.overall(), Overall::NoRuns);
        assert!(!snap.overall().is_terminal());
    }

    #[test]
    fn runノード応答を正規化する() {
        let v = json!({"data":{"node":{"checkSuite":{
            "repository":{"nameWithOwner":"octo/demo"},
            "commit":{"oid":"deadbeef"},
            "app":{"name":"GitHub Actions"},"status":"COMPLETED","conclusion":"FAILURE",
            "workflowRun":{"databaseId":55,"runNumber":3,"event":"push","url":"u","workflow":{"name":"Build"}},
            "checkRuns":{"pageInfo":{"hasNextPage":true},"nodes":[
              {"name":"j","status":"COMPLETED","conclusion":"FAILURE","detailsUrl":null,"startedAt":null,"completedAt":null}]}
        }},"rateLimit":{"cost":1,"remaining":10,"resetAt":"2026-11-16T13:00:00Z"}}});
        let snap = parse_run_response(&v, 55).unwrap();
        assert_eq!(snap.repo, "octo/demo");
        assert_eq!(snap.overall(), Overall::Failure);
        assert!(snap.truncated);
        assert_eq!(snap.suites[0].run_id, Some(55));
    }

    #[test]
    fn 存在しない対象はnot_found() {
        let v = json!({"data":{"repository":null},"errors":[
            {"type":"NOT_FOUND","message":"Could not resolve to a Repository"}]});
        assert!(matches!(
            parse_commit_response(&v, "x"),
            Err(VigiliaError::NotFound(_))
        ));
        let v = json!({"data":{"repository":{"nameWithOwner":"a/b","object":null}}});
        assert!(matches!(
            parse_commit_response(&v, "x"),
            Err(VigiliaError::NotFound(_))
        ));
        let v = json!({"data":{"node":null}});
        assert!(matches!(
            parse_run_response(&v, 1),
            Err(VigiliaError::NotFound(_))
        ));
    }

    #[test]
    fn レート制限エラーを識別する() {
        let v = json!({"errors":[{"type":"RATE_LIMITED","message":"API rate limit exceeded"}]});
        assert!(matches!(
            parse_commit_response(&v, "x"),
            Err(VigiliaError::RateLimited { .. })
        ));
    }

    #[test]
    fn その他のgraphqlエラーはapiエラー() {
        let v = json!({"errors":[{"message":"boom"}]});
        assert!(matches!(
            parse_commit_response(&v, "x"),
            Err(VigiliaError::Api(_))
        ));
    }

    // --- workflow / event 絞り込みと run 一覧 ---

    /// 実在コミット相当の応答(Release・CI・Dependabot 2 本・CI の再実行 / dispatch 重複)。
    fn runs_fixture() -> serde_json::Value {
        let suite = |id: u64, num: u64, attempt: u64, name: &str, path: Option<&str>, event: &str,
                     created: &str, branch: Option<&str>, concl: &str| {
            json!({
              "app": {"name": "GitHub Actions"}, "status": "COMPLETED", "conclusion": concl,
              "branch": branch.map(|b| json!({"name": b})),
              "workflowRun": {"databaseId": id, "runNumber": num, "runAttempt": attempt,
                              "createdAt": created, "event": event,
                              "url": format!("https://github.com/octo/demo/actions/runs/{id}"),
                              "workflow": {"name": name},
                              "file": path.map(|p| json!({"path": p}))},
              "checkRuns": {"pageInfo":{"hasNextPage":false},"nodes":[]}
            })
        };
        json!({"data":{"repository":{"nameWithOwner":"octo/demo","object":{
            "oid":"abeebf669a1fbb239d7bf537c88293d0b98177af",
            "checkSuites":{"pageInfo":{"hasNextPage":false},"nodes":[
              suite(10, 1, 1, "Release", Some(".github/workflows/release.yml"), "push",
                    "2026-11-17T18:17:24Z", Some("release/1.0.1"), "SUCCESS"),
              suite(11, 6, 1, "CI", Some(".github/workflows/ci.yml"), "push",
                    "2026-11-17T18:17:12Z", Some("main"), "FAILURE"),
              // CI の新しい run(前の失敗に取って代わる)。
              suite(12, 7, 1, "CI", Some(".github/workflows/ci.yml"), "push",
                    "2026-11-17T18:20:00Z", Some("main"), "SUCCESS"),
              // 同じ CI の手動実行(event が違う)。
              suite(13, 8, 1, "CI", Some(".github/workflows/ci.yml"), "workflow_dispatch",
                    "2026-11-17T18:25:00Z", Some("main"), "CANCELLED"),
              // パスを持たない動的 workflow。
              suite(14, 1, 1, "Dependabot Updates", None, "dynamic",
                    "2026-11-17T18:17:17Z", Some("main"), "SUCCESS")
            ]}}},
          "rateLimit":{"cost":1,"remaining":4949,"resetAt":"2026-11-17T19:23:25Z"}}})
    }

    fn wf(spec: &str) -> RunFilter {
        RunFilter {
            workflow: Some(spec.into()),
            ..Default::default()
        }
    }

    #[test]
    fn run一覧は作成時刻の新しい順で決定的に並ぶ() {
        let snap = parse_commit_response(&runs_fixture(), "x").unwrap();
        let runs = snap.runs(&RunFilter::default());
        let ids: Vec<u64> = runs.iter().map(|r| r.run_id).collect();
        assert_eq!(ids, vec![13, 12, 10, 14, 11]);
        assert_eq!(runs[2].workflow_path.as_deref(), Some(".github/workflows/release.yml"));
        assert_eq!(runs[2].status, "completed");
        assert_eq!(runs[2].conclusion.as_deref(), Some("success"));
        assert_eq!(runs[2].head_sha, "abeebf669a1fbb239d7bf537c88293d0b98177af");
        assert_eq!(runs[2].branch.as_deref(), Some("release/1.0.1"));
        assert_eq!(runs[2].run_attempt, Some(1));
        // 同じ入力なら同じ並び。
        assert_eq!(runs, snap.runs(&RunFilter::default()));
    }

    #[test]
    fn workflowはパス_ファイル名_表示名のどれでも一致する() {
        let snap = parse_commit_response(&runs_fixture(), "x").unwrap();
        for spec in [".github/workflows/release.yml", "release.yml", "Release"] {
            let ids: Vec<u64> = snap.runs(&wf(spec)).iter().map(|r| r.run_id).collect();
            assert_eq!(ids, vec![10], "{spec}");
        }
        // 大文字小文字は区別する・部分一致はしない。
        assert!(snap.runs(&wf("release")).is_empty());
        assert!(snap.runs(&wf("RELEASE.YML")).is_empty());
        // パスを持たない workflow は表示名で指定できる。
        assert_eq!(snap.runs(&wf("Dependabot Updates")).len(), 1);
    }

    #[test]
    fn eventの絞り込みは大文字小文字を区別しない() {
        let snap = parse_commit_response(&runs_fixture(), "x").unwrap();
        let f = RunFilter {
            workflow: Some("ci.yml".into()),
            event: Some("Workflow_Dispatch".into()),
            since: None,
        };
        let ids: Vec<u64> = snap.runs(&f).iter().map(|r| r.run_id).collect();
        assert_eq!(ids, vec![13]);
    }

    #[test]
    fn sinceは作成時刻以後だけを残す() {
        let snap = parse_commit_response(&runs_fixture(), "x").unwrap();
        let f = RunFilter {
            since: DateTime::parse_from_rfc3339("2026-11-17T18:17:24Z")
                .ok()
                .map(|d| d.with_timezone(&Utc)),
            ..Default::default()
        };
        // ちょうど同時刻は含める(以後)。
        let ids: Vec<u64> = snap.runs(&f).iter().map(|r| r.run_id).collect();
        assert_eq!(ids, vec![13, 12, 10]);
    }

    #[test]
    fn 同じworkflowとeventの複数runは最新だけで結論を出す() {
        let snap = parse_commit_response(&runs_fixture(), "x").unwrap();
        // push だけ: 古い失敗(#6)は新しい成功(#7)に取って代わられ、成功になる。
        let f = RunFilter {
            workflow: Some("ci.yml".into()),
            event: Some("push".into()),
            since: None,
        };
        let s = snap.filtered(&f).unwrap();
        assert_eq!(s.suites.len(), 1);
        assert_eq!(s.suites[0].run_id, Some(12));
        assert_eq!(s.overall(), Overall::Success);
        // event を指定しなければ push と dispatch の両方を結論に含める(取消が効く)。
        let s = snap.filtered(&wf("ci.yml")).unwrap();
        let ids: Vec<u64> = s.suites.iter().map(|x| x.run_id.unwrap()).collect();
        assert_eq!(ids, vec![12, 13]);
        assert_eq!(s.overall(), Overall::Cancelled);
    }

    #[test]
    fn 再実行のattemptは同じrunなので1件として扱う() {
        // 再実行は run id が同じまま attempt が増える。一覧には最新 attempt の 1 件だけが出る。
        let mut v = runs_fixture();
        v["data"]["repository"]["object"]["checkSuites"]["nodes"][0]["workflowRun"]["runAttempt"] =
            json!(2);
        let snap = parse_commit_response(&v, "x").unwrap();
        let r = &snap.runs(&wf("release.yml"))[0];
        assert_eq!(r.run_attempt, Some(2));
        assert_eq!(snap.runs(&wf("release.yml")).len(), 1);
    }

    #[test]
    fn 一致するrunが無ければrunなし() {
        let snap = parse_commit_response(&runs_fixture(), "x").unwrap();
        let s = snap.filtered(&wf("nope.yml")).unwrap();
        assert_eq!(s.overall(), Overall::NoRuns);
        assert!(s.conclusive_overall().is_ok());
    }

    #[test]
    fn 複数のworkflowに一致する指定は使用エラー() {
        // 表示名 "CI" が別ファイルの workflow とも一致する場合は決められない。
        let mut v = runs_fixture();
        v["data"]["repository"]["object"]["checkSuites"]["nodes"][0]["workflowRun"]["workflow"]["name"] =
            json!("CI");
        let snap = parse_commit_response(&v, "x").unwrap();
        match snap.filtered(&wf("CI")) {
            Err(VigiliaError::Usage(m)) => {
                assert!(m.contains("release.yml") && m.contains("ci.yml"), "{m}");
            }
            other => panic!("使用エラーのはず: {other:?}"),
        }
        // ファイルパスで指定すれば曖昧さは無い。
        assert!(snap.filtered(&wf("ci.yml")).is_ok());
        // runs の一覧は曖昧でもエラーにせず全て返す。
        assert_eq!(snap.runs(&wf("CI")).len(), 5 - 1);
    }

    #[test]
    fn 絞り込み後の未完了は実行中になる() {
        let mut v = runs_fixture();
        v["data"]["repository"]["object"]["checkSuites"]["nodes"][0]["status"] =
            json!("IN_PROGRESS");
        v["data"]["repository"]["object"]["checkSuites"]["nodes"][0]["conclusion"] =
            serde_json::Value::Null;
        let snap = parse_commit_response(&v, "x").unwrap();
        assert_eq!(snap.filtered(&wf("release.yml")).unwrap().overall(), Overall::InProgress);
        // 他の workflow の未完了は影響しない(CI だけなら完了済み)。
        assert!(snap.filtered(&wf("ci.yml")).unwrap().overall().is_terminal());
    }

    #[test]
    fn 旧形式のキャッシュにも新フィールドは既定値で読める() {
        let s: Suite = serde_json::from_value(json!({
            "app":"a","workflow":"CI","run_id":1,"run_number":1,"event":"push","url":null,
            "status":"COMPLETED","conclusion":"SUCCESS","jobs":[]
        }))
        .unwrap();
        assert_eq!(s.workflow_path, None);
        assert_eq!(s.created_at, None);
    }
}
