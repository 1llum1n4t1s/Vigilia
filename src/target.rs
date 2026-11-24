//! 監視対象(run id / sha)とリポジトリ指定の解釈・検証。

use crate::error::{Result, VigiliaError};
use std::process::Command;

/// 監視対象。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// GitHub Actions の workflow run ID(databaseId)。
    Run(u64),
    /// コミット SHA(短縮可。小文字へ正規化済み)。
    Sha(String),
    /// ブランチ名(`runs` 専用。先頭コミットを GraphQL で解決する)。
    Branch(String),
}

impl Target {
    /// GraphQL の `object(expression:)` に渡す式。ブランチは同名タグと衝突しないよう
    /// 完全な ref 名にする。
    pub fn expression(&self) -> String {
        match self {
            Target::Sha(s) => s.clone(),
            Target::Branch(b) if b.starts_with("refs/") => b.clone(),
            Target::Branch(b) => format!("refs/heads/{b}"),
            Target::Run(id) => id.to_string(),
        }
    }
}

/// `runs` 用の対象解釈。`HEAD`・SHA・ブランチ名を受け付け、run id は受け付けない。
/// SHA として読める文字列(16 進 7〜40 桁)は SHA を優先する。
pub fn parse_runs_target(input: &str) -> Result<Target> {
    let s = input.trim();
    if s.is_empty() {
        return Err(VigiliaError::Usage("対象が空です".into()));
    }
    if s.eq_ignore_ascii_case("HEAD") {
        return resolve_head();
    }
    match classify(s) {
        Ok(Target::Run(_)) => Err(VigiliaError::Usage(
            "runs は run id を指定できません。コミット SHA・HEAD・ブランチ名を指定してください".into(),
        )),
        Ok(t) => Ok(t),
        Err(_) => {
            // git の ref 名として明らかに不正なものだけ弾く(最終的な解決は GitHub が行う)。
            let bad = s.starts_with('-')
                || s.contains("..")
                || s.chars().any(|c| c.is_whitespace() || c.is_control() || "~^:?*[\\".contains(c));
            if bad {
                return Err(VigiliaError::Usage(format!(
                    "対象 '{s}' を解釈できません。SHA・HEAD・ブランチ名を指定してください"
                )));
            }
            Ok(Target::Branch(s.to_string()))
        }
    }
}

/// 数字のみで 9 桁以上なら run id とみなす。
/// 現在の run id は 11 桁前後で、全桁が数字の短縮 SHA(7〜8 桁)と衝突しにくい境界。
const RUN_ID_MIN_DIGITS: usize = 9;

/// 文字列を対象へ分類する。`HEAD` はカレントの git から解決する。
pub fn parse_target(input: &str) -> Result<Target> {
    let s = input.trim();
    if s.is_empty() {
        return Err(VigiliaError::Usage("対象が空です".into()));
    }
    if s.eq_ignore_ascii_case("HEAD") {
        return resolve_head();
    }
    classify(s)
}

fn classify(s: &str) -> Result<Target> {
    if s.chars().all(|c| c.is_ascii_digit()) && s.len() >= RUN_ID_MIN_DIGITS {
        return s
            .parse::<u64>()
            .map(Target::Run)
            .map_err(|_| VigiliaError::Usage(format!("run id が大きすぎます: {s}")));
    }
    if (7..=40).contains(&s.len()) && s.chars().all(|c| c.is_ascii_hexdigit()) {
        return Ok(Target::Sha(s.to_ascii_lowercase()));
    }
    Err(VigiliaError::Usage(format!(
        "対象 '{s}' を解釈できません。run id(数字 9 桁以上)か SHA(16 進 7〜40 桁)、または HEAD を指定してください"
    )))
}

fn resolve_head() -> Result<Target> {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .map_err(|e| VigiliaError::Usage(format!("git を実行できません: {e}")))?;
    if !out.status.success() {
        return Err(VigiliaError::Usage(
            "HEAD を解決できません。git リポジトリ内で実行するか、SHA を指定してください".into(),
        ));
    }
    classify(String::from_utf8_lossy(&out.stdout).trim())
}

/// `OWNER/REPO` 形式を検証して (owner, repo) を返す。
pub fn parse_repo(s: &str) -> Result<(String, String)> {
    let bad = || VigiliaError::Usage(format!("--repo は OWNER/REPO 形式で指定してください: '{s}'"));
    let (owner, repo) = s.trim().split_once('/').ok_or_else(bad)?;
    let valid = |p: &str| {
        !p.is_empty()
            && p.len() <= 100
            && p
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    };
    if !valid(owner) || !valid(repo) || repo.contains('/') {
        return Err(bad());
    }
    Ok((owner.to_string(), repo.to_string()))
}

/// git の remote URL(SSH / HTTPS)から github.com の (owner, repo) を取り出す。
/// ホストは `github.com` と、ポート 443 の SSH 用の `ssh.github.com` だけを受け付ける。
pub fn parse_github_remote(url: &str) -> Option<(String, String)> {
    let url = url.trim();
    // scp 風: git@github.com:owner/repo.git
    // URL 風: ssh://git@github.com/owner/repo.git, https://github.com/owner/repo(.git)
    let path = if let Some(rest) = url
        .strip_prefix("git@github.com:")
        .or_else(|| url.strip_prefix("git@ssh.github.com:"))
    {
        rest
    } else {
        let after_scheme = url.split_once("://").map(|(_, r)| r)?;
        // userinfo を除去してホストとパスへ分割
        let after_user = after_scheme.rsplit_once('@').map(|(_, r)| r).unwrap_or(after_scheme);
        let (host, path) = after_user.split_once('/')?;
        // ポート指定(ssh://...:22)は無視
        let host = host.split(':').next().unwrap_or(host);
        // `ssh.github.com`(ポート 443 の SSH。例: ssh://git@ssh.github.com:443/o/r.git)は
        // github.com への SSH 接続の別口なので github.com として扱う。GitHub Enterprise は対象外。
        if !host.eq_ignore_ascii_case("github.com") && !host.eq_ignore_ascii_case("ssh.github.com") {
            return None;
        }
        path
    };
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, repo) = path.split_once('/')?;
    parse_repo(&format!("{owner}/{repo}")).ok()
}

/// カレントディレクトリの git remote から github.com のリポジトリを推定する。
pub fn infer_repo() -> Result<(String, String)> {
    let hint = "--repo OWNER/REPO を指定してください";
    let remotes = Command::new("git")
        .args(["remote"])
        .output()
        .map_err(|e| VigiliaError::Usage(format!("git を実行できません: {e}。{hint}")))?;
    if !remotes.status.success() {
        return Err(VigiliaError::Usage(format!(
            "git リポジトリ内ではないためリポジトリを推定できません。{hint}"
        )));
    }
    let names = String::from_utf8_lossy(&remotes.stdout).to_string();
    // origin を最優先、無ければ列挙順。
    let mut ordered: Vec<&str> = names.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    ordered.sort_by_key(|n| if *n == "origin" { 0 } else { 1 });
    for name in ordered {
        let out = Command::new("git").args(["remote", "get-url", name]).output();
        if let Ok(out) = out
            && out.status.success()
            && let Some(r) = parse_github_remote(&String::from_utf8_lossy(&out.stdout))
        {
            return Ok(r);
        }
    }
    Err(VigiliaError::Usage(format!(
        "github.com の remote が見つかりません。{hint}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 対象の分類() {
        assert_eq!(
            classify("43778559656").unwrap(),
            Target::Run(43778559656)
        );
        assert_eq!(
            classify("E8C8BB3").unwrap(),
            Target::Sha("e8c8bb3".into())
        );
        // 数字のみ: 8 桁は短縮 SHA、9 桁は run id。
        assert_eq!(classify("12345678").unwrap(), Target::Sha("12345678".into()));
        assert_eq!(classify("123456789").unwrap(), Target::Run(123456789));
        // 16 進: 6 桁は短すぎ、7 桁と 40 桁は SHA、41 桁は長すぎ。
        assert!(classify("abcdef").is_err());
        assert_eq!(classify("abcdef0").unwrap(), Target::Sha("abcdef0".into()));
        assert!(classify(&"a".repeat(40)).is_ok());
        assert!(classify(&"a".repeat(41)).is_err());
        assert!(classify("xyz").is_err());
    }

    #[test]
    fn remote_url解析() {
        let want = Some(("o".to_string(), "r".to_string()));
        assert_eq!(parse_github_remote("git@github.com:o/r.git"), want);
        assert_eq!(parse_github_remote("https://github.com/o/r"), want);
        assert_eq!(parse_github_remote("https://github.com/o/r.git/"), want);
        assert_eq!(parse_github_remote("https://user:tok@github.com/o/r.git"), want);
        assert_eq!(parse_github_remote("ssh://git@github.com:22/o/r.git"), want);
        // ポート 443 の SSH(ssh.github.com)も github.com として受理する。
        assert_eq!(parse_github_remote("ssh://git@ssh.github.com:443/o/r.git"), want);
        assert_eq!(parse_github_remote("ssh://git@SSH.GitHub.com:443/o/r"), want);
        // scp 形式の ssh.github.com も受理する。
        assert_eq!(parse_github_remote("git@ssh.github.com:o/r.git"), want);
        assert_eq!(parse_github_remote("git@ssh.github.example.com:o/r.git"), None);
        // 別ホスト(GitHub Enterprise・似た名前)は受理しない。
        assert_eq!(parse_github_remote("ssh://git@ssh.github.example.com:443/o/r.git"), None);
        assert_eq!(parse_github_remote("ssh://git@evilgithub.com/o/r.git"), None);
        assert_eq!(parse_github_remote("https://gitlab.com/o/r.git"), None);
        assert_eq!(parse_github_remote("not a url"), None);
    }

    #[test]
    fn runs対象はshaとブランチを受け付けrun_idは拒否する() {
        assert_eq!(
            parse_runs_target("E8C8BB3").unwrap(),
            Target::Sha("e8c8bb3".into())
        );
        assert_eq!(
            parse_runs_target("release/1.0.1").unwrap(),
            Target::Branch("release/1.0.1".into())
        );
        assert_eq!(parse_runs_target("main").unwrap(), Target::Branch("main".into()));
        // 9 桁以上の数字だけは run id なので拒否、8 桁は短縮 SHA。
        assert!(parse_runs_target("123456789").is_err());
        assert!(parse_runs_target("12345678").is_ok());
        assert!(parse_runs_target("-x").is_err());
        assert!(parse_runs_target("a..b").is_err());
        assert!(parse_runs_target("a b").is_err());
        assert!(parse_runs_target("").is_err());
    }

    #[test]
    fn ブランチは完全なref名で解決する() {
        assert_eq!(
            Target::Branch("main".into()).expression(),
            "refs/heads/main"
        );
        assert_eq!(
            Target::Branch("refs/heads/x".into()).expression(),
            "refs/heads/x"
        );
        assert_eq!(Target::Sha("abc1234".into()).expression(), "abc1234");
    }
}
