//! CI 状態のドメインモデルと、GraphQL レスポンスからの変換。
//!
//! 設計意図:
//! - GraphQL の生レスポンス型(`Raw*`)と、キャッシュ/JSON 出力に使う
//!   正規化済みの `Snapshot` を分離する。キャッシュ形式を API 都合から切り離すため。
//! - 「結論(Overall)」の判定はここに集約し、`watch` と `status` で同じ規則を使う。

use crate::error::{Result, VigiliaError};
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Suite {
    pub app: String,
    pub workflow: Option<String>,
    pub run_id: Option<u64>,
    pub run_number: Option<u64>,
    pub event: Option<String>,
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
}

#[derive(Debug, Deserialize)]
struct RawApp {
    name: String,
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
        run_id: run.as_ref().and_then(|r| r.database_id),
        run_number: run.as_ref().and_then(|r| r.run_number),
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
}
