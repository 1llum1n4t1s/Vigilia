//! GitHub API クライアント(GraphQL 取得 + `/rate_limit` 参照)。
//!
//! 重要な設計方針:
//! - CI 状態の取得は **GraphQL のみ**。REST のコア枠(5,000 回/時)は他のセッションの
//!   `gh run watch` 等が使い切っていても、GraphQL の枠は別に数えられるため動き続ける。
//! - `GET /rate_limit` は枠を消費しないため、`budget` で安全に使える。
//! - リクエストは常に直列(並列にしない)。二次レート制限(短時間の同時接続・過剰頻度)を避ける。
//! - トークンは `GH_TOKEN` / `GITHUB_TOKEN`、無ければ `gh auth token` から借りる。
//!   値はメモリ上でのみ保持し、ログ・エラー文・出力へは決して出さない。

use crate::error::{Result, VigiliaError};
use crate::model::{self, Snapshot};
use crate::target::Target;
use reqwest::StatusCode;
use reqwest::blocking::Client as Http;
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::process::Command;
use std::time::Duration;

const GRAPHQL_URL: &str = "https://api.github.com/graphql";
const RATE_LIMIT_URL: &str = "https://api.github.com/rate_limit";

/// レート制限に当たったが待ち時間が分からないときの既定待機。
const DEFAULT_RATE_WAIT: Duration = Duration::from_secs(60);

/// 一時的な失敗(通信エラー・5xx)の最大試行回数。
const MAX_ATTEMPTS: u32 = 3;

// sha 起点: checkSuites を 1 クエリで取得する。
// コストは「必要なリクエスト数/100」で数えられるため、suite 50 × job 30 でも cost は小さい
// (実測で確認すること)。超過は pageInfo で検知する。suite の切り捨ては結論を保証できないため、
// 成功扱いにせずエラーにする(commands 側)。job の切り捨ては表示上の注意のみ。
const COMMIT_QUERY: &str = r#"
query($owner:String!,$name:String!,$expr:String!){
  repository(owner:$owner,name:$name){
    nameWithOwner
    object(expression:$expr){
      ... on Commit {
        oid
        checkSuites(first:50){
          pageInfo{hasNextPage}
          nodes{
            app{name} status conclusion
            workflowRun{databaseId runNumber event url workflow{name}}
            checkRuns(first:30){
              pageInfo{hasNextPage}
              nodes{name status conclusion detailsUrl startedAt completedAt}
            }
          }
        }
      }
    }
  }
  rateLimit{cost remaining resetAt}
}"#;

// run id 起点: 1 つの WorkflowRun から check suite を辿る。
const RUN_QUERY: &str = r#"
query($id:ID!){
  node(id:$id){
    ... on WorkflowRun {
      checkSuite{
        repository{nameWithOwner}
        commit{oid}
        app{name} status conclusion
        workflowRun{databaseId runNumber event url workflow{name}}
        checkRuns(first:50){
          pageInfo{hasNextPage}
          nodes{name status conclusion detailsUrl startedAt completedAt}
        }
      }
    }
  }
  rateLimit{cost remaining resetAt}
}"#;

/// 認証トークンを取得する(値は出力しない)。
fn load_token() -> Result<String> {
    for var in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(v) = std::env::var(var) {
            let v = v.trim().to_string();
            if !v.is_empty() {
                return Ok(v);
            }
        }
    }
    let out = Command::new("gh")
        .args(["auth", "token", "--hostname", "github.com"])
        .output()
        .map_err(|e| {
            VigiliaError::Auth(format!(
                "gh コマンドを実行できません({e})。GH_TOKEN を設定するか gh をインストールしてください"
            ))
        })?;
    let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || token.is_empty() {
        return Err(VigiliaError::Auth(
            "トークンを取得できません。`gh auth login` を実行するか GH_TOKEN を設定してください".into(),
        ));
    }
    Ok(token)
}

pub struct Client {
    http: Http,
    token: String,
}

impl Client {
    pub fn new() -> Result<Self> {
        let token = load_token()?;
        let http = Http::builder()
            .user_agent(concat!("vigilia/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| VigiliaError::Network(e.to_string()))?;
        Ok(Client { http, token })
    }

    /// GraphQL を 1 回実行して JSON を返す。一時的失敗は指数バックオフで再試行する。
    pub fn graphql(&self, query: &str, variables: Value) -> Result<Value> {
        let body = json!({ "query": query, "variables": variables });
        let mut attempt = 0;
        loop {
            attempt += 1;
            let sent = self
                .http
                .post(GRAPHQL_URL)
                .bearer_auth(&self.token)
                .json(&body)
                .send();
            let resp = match sent {
                Ok(r) => r,
                Err(e) => {
                    if attempt < MAX_ATTEMPTS {
                        backoff(attempt);
                        continue;
                    }
                    return Err(VigiliaError::Network(strip_url(e)));
                }
            };
            let status = resp.status();
            let headers = resp.headers().clone();
            let text = resp.text().unwrap_or_default();

            if status.is_server_error() {
                if attempt < MAX_ATTEMPTS {
                    backoff(attempt);
                    continue;
                }
                return Err(VigiliaError::Api(format!("HTTP {status}(サーバーエラー)")));
            }
            if let Some(err) = classify_http_error(status, &headers, &text) {
                return Err(err);
            }
            let value: Value = serde_json::from_str(&text).map_err(|e| {
                VigiliaError::Api(format!("JSON として解釈できないレスポンスです: {e}"))
            })?;
            // 200 でも errors に RATE_LIMITED が入るため、待ち時間をヘッダーから補う。
            return match model::check_gql_errors(&value) {
                Err(VigiliaError::RateLimited {
                    retry_after: None,
                    message,
                }) => Err(VigiliaError::RateLimited {
                    retry_after: Some(
                        wait_from_headers(&headers, crate::cache::now_ms() / 1000)
                            .unwrap_or(DEFAULT_RATE_WAIT),
                    ),
                    message,
                }),
                _ => Ok(value),
            };
        }
    }

    /// 対象の CI 状態を GraphQL で 1 回取得する(キャッシュは扱わない)。
    pub fn fetch_snapshot(
        &self,
        repo: Option<&(String, String)>,
        target: &Target,
    ) -> Result<Snapshot> {
        match target {
            Target::Sha(sha) => {
                let (owner, name) = repo.ok_or_else(|| {
                    VigiliaError::Usage("SHA の場合はリポジトリが必要です".into())
                })?;
                let v = self.graphql(
                    COMMIT_QUERY,
                    json!({ "owner": owner, "name": name, "expr": sha }),
                )?;
                model::parse_commit_response(&v, sha)
            }
            Target::Run(id) => {
                let v = self.graphql(RUN_QUERY, json!({ "id": legacy_run_node_id(*id) }))?;
                let snap = model::parse_run_response(&v, *id)?;
                if let Some((o, n)) = repo {
                    let want = format!("{o}/{n}");
                    if !snap.repo.eq_ignore_ascii_case(&want) {
                        return Err(VigiliaError::Usage(format!(
                            "run {id} は {} のものです(--repo {want} と一致しません)",
                            snap.repo
                        )));
                    }
                }
                Ok(snap)
            }
        }
    }

    /// `GET /rate_limit`(この呼び出し自体は枠を消費しない)。
    pub fn rate_limits(&self) -> Result<RateLimits> {
        let resp = self
            .http
            .get(RATE_LIMIT_URL)
            .bearer_auth(&self.token)
            .header("Accept", "application/vnd.github+json")
            .send()
            .map_err(|e| VigiliaError::Network(strip_url(e)))?;
        let status = resp.status();
        let headers = resp.headers().clone();
        let text = resp.text().unwrap_or_default();
        if let Some(err) = classify_http_error(status, &headers, &text) {
            return Err(err);
        }
        let parsed: RateLimitResponse = serde_json::from_str(&text)
            .map_err(|e| VigiliaError::Api(format!("rate_limit を解釈できません: {e}")))?;
        Ok(parsed.resources)
    }
}

/// run の databaseId からレガシー形式のグローバル node ID を作る。
/// 形式: base64("011:WorkflowRun" + id)。`011` は型名 "WorkflowRun" の長さ。
/// レガシー ID は GitHub が非推奨としているが、現状は node(id:) で引ける。
/// (REST を使わずに run id から run を引ける唯一の経路。将来廃止された場合は
///  API エラーとして表面化するので README の注意書きを参照。)
pub fn legacy_run_node_id(run_id: u64) -> String {
    base64_encode(format!("011:WorkflowRun{run_id}").as_bytes())
}

/// 依存を増やさないための最小 base64(標準アルファベット、パディングあり)。
fn base64_encode(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

fn backoff(attempt: u32) {
    std::thread::sleep(Duration::from_secs(1 << (attempt - 1)));
}

/// reqwest のエラーから URL(クエリ等)を除いた短い説明を作る。
fn strip_url(e: reqwest::Error) -> String {
    e.without_url().to_string()
}

/// ヘッダーから待ち時間を求める。`Retry-After`(秒)を最優先し、
/// 無ければ残り 0 のときの `x-ratelimit-reset`(epoch 秒)までの差。
pub fn wait_from_headers(headers: &HeaderMap, now_epoch_secs: u64) -> Option<Duration> {
    let get = |k: &str| headers.get(k).and_then(|v| v.to_str().ok());
    if let Some(secs) = get("retry-after").and_then(|v| v.trim().parse::<u64>().ok()) {
        return Some(Duration::from_secs(secs.max(1)));
    }
    let remaining = get("x-ratelimit-remaining").and_then(|v| v.trim().parse::<u64>().ok());
    let reset = get("x-ratelimit-reset").and_then(|v| v.trim().parse::<u64>().ok());
    if let (Some(0), Some(reset)) = (remaining, reset) {
        return Some(Duration::from_secs(reset.saturating_sub(now_epoch_secs).max(1)));
    }
    None
}

/// HTTP ステータスからエラーを分類する。成功(2xx)なら None。
pub fn classify_http_error(status: StatusCode, headers: &HeaderMap, body: &str) -> Option<VigiliaError> {
    if status.is_success() {
        return None;
    }
    let now = crate::cache::now_ms() / 1000;
    let lower = body.to_ascii_lowercase();
    let rate_like = lower.contains("rate limit") || lower.contains("abuse");
    match status {
        StatusCode::UNAUTHORIZED => Some(VigiliaError::Auth(
            "トークンが無効または期限切れです。`gh auth login` で再認証してください".into(),
        )),
        StatusCode::TOO_MANY_REQUESTS => Some(VigiliaError::RateLimited {
            retry_after: Some(wait_from_headers(headers, now).unwrap_or(DEFAULT_RATE_WAIT)),
            message: "HTTP 429".into(),
        }),
        StatusCode::FORBIDDEN => {
            let wait = wait_from_headers(headers, now);
            if wait.is_some() || rate_like {
                Some(VigiliaError::RateLimited {
                    retry_after: Some(wait.unwrap_or(DEFAULT_RATE_WAIT)),
                    message: "HTTP 403(レート制限)".into(),
                })
            } else {
                Some(VigiliaError::Auth(
                    "HTTP 403: 権限がありません(トークンのスコープ・リポジトリへのアクセスを確認してください)".into(),
                ))
            }
        }
        StatusCode::NOT_FOUND => Some(VigiliaError::NotFound(
            "HTTP 404: 対象が見つかりません(存在しないか権限がありません)".into(),
        )),
        s => Some(VigiliaError::Api(format!("HTTP {s}"))),
    }
}

#[derive(Debug, Deserialize)]
struct RateLimitResponse {
    resources: RateLimits,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimits {
    pub core: Option<Resource>,
    pub graphql: Option<Resource>,
    pub search: Option<Resource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resource {
    pub limit: u64,
    pub remaining: u64,
    pub used: u64,
    /// リセット時刻(UNIX epoch 秒)。
    pub reset: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderName, HeaderValue};

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    #[test]
    fn レガシーnode_idの生成() {
        // 実在する run(43778559656)で動作確認済みの値。
        assert_eq!(
            legacy_run_node_id(43778559656),
            "MDExOldvcmtmbG93UnVuNDM3Nzg1NTk2NTY="
        );
    }

    #[test]
    fn retry_afterを最優先する() {
        let h = headers(&[
            ("retry-after", "30"),
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", "2000"),
        ]);
        assert_eq!(wait_from_headers(&h, 1000), Some(Duration::from_secs(30)));
    }

    #[test]
    fn 残り0ならresetまでの差() {
        let h = headers(&[("x-ratelimit-remaining", "0"), ("x-ratelimit-reset", "1090")]);
        assert_eq!(wait_from_headers(&h, 1000), Some(Duration::from_secs(90)));
        // 既に過ぎていても最低 1 秒。
        assert_eq!(wait_from_headers(&h, 5000), Some(Duration::from_secs(1)));
        // 残りがあれば待ち不要。
        let h = headers(&[("x-ratelimit-remaining", "10"), ("x-ratelimit-reset", "1090")]);
        assert_eq!(wait_from_headers(&h, 1000), None);
    }

    #[test]
    fn httpエラー分類() {
        let none = HeaderMap::new();
        assert!(classify_http_error(StatusCode::OK, &none, "").is_none());
        assert!(matches!(
            classify_http_error(StatusCode::UNAUTHORIZED, &none, ""),
            Some(VigiliaError::Auth(_))
        ));
        assert!(matches!(
            classify_http_error(StatusCode::TOO_MANY_REQUESTS, &none, ""),
            Some(VigiliaError::RateLimited { .. })
        ));
        // 403 + 本文に rate limit → レート制限。
        assert!(matches!(
            classify_http_error(StatusCode::FORBIDDEN, &none, "API rate limit exceeded"),
            Some(VigiliaError::RateLimited { .. })
        ));
        // 403 で理由なし → 認可エラー。
        assert!(matches!(
            classify_http_error(StatusCode::FORBIDDEN, &none, "Resource not accessible"),
            Some(VigiliaError::Auth(_))
        ));
        assert!(matches!(
            classify_http_error(StatusCode::NOT_FOUND, &none, ""),
            Some(VigiliaError::NotFound(_))
        ));
    }
}
