//! Vigilia(ラテン語で「見張り」): GitHub REST 枠を使わずに CI 状態を見張るツール。
//!
//! 背景: 複数セッションの `gh run watch` 等の polling が REST の枠(認証ユーザー 5,000 回/時)を
//! 使い切ると `HTTP 403: API rate limit exceeded` になる。Vigilia は別枠の GraphQL で状態を取得し、
//! 結果をファイルキャッシュで共有して、問い合わせ自体を減らす。

mod cache;
mod commands;
mod error;
mod github;
mod model;
mod render;
mod service;
mod target;

use clap::{Args, Parser, Subcommand};
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "vigilia",
    version,
    about = "REST 枠を使わず GraphQL で GitHub Actions の状態を見張る(結果は全セッションで共有)",
    long_about = None
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 完了まで監視する(成功 0 / 失敗 1 / 取消 3 / タイムアウト 4 / エラー 2 / レート制限 5 / 未検出 7)
    Watch(WatchArgs),
    /// 状態を 1 回だけ取得する
    Status(StatusArgs),
    /// コミット(SHA・HEAD・ブランチ)の workflow run 一覧を表示する(見つからなければ 7)
    Runs(RunsArgs),
    /// REST / GraphQL / search の残り枠とリセット時刻を表示する(枠は消費しない)
    Budget(BudgetArgs),
}

/// 対象の指定と取得方法(watch / status 共通)。
#[derive(Args)]
pub struct TargetArgs {
    /// workflow run ID(数字 9 桁以上)または コミット SHA(7〜40 桁)。省略時は HEAD
    pub target: Option<String>,
    /// OWNER/REPO。省略時はカレントの git remote から推定
    #[arg(long)]
    pub repo: Option<String>,
    /// キャッシュを新鮮とみなす秒数(他セッションの取得結果をこの間は再利用する)
    #[arg(long, default_value_t = 15)]
    pub ttl: u64,
    /// 共有キャッシュを使わず毎回取得する
    #[arg(long)]
    pub no_cache: bool,
    /// SHA / HEAD 指定のとき、この workflow の run だけで判定する(表示名、
    /// または `.github/workflows/x.yml` / `x.yml` のパス)。run が現れるまでは待機 / 未検出
    #[arg(long)]
    pub workflow: Option<String>,
    /// SHA / HEAD 指定のとき、このイベント(push / workflow_dispatch など)の run だけで判定する
    #[arg(long)]
    pub event: Option<String>,
}

#[derive(Args)]
pub struct WatchArgs {
    #[command(flatten)]
    pub target: TargetArgs,
    /// 取得間隔(秒、5 以上)
    #[arg(long, default_value_t = 20)]
    pub interval: u64,
    /// 監視を打ち切るまでの秒数
    #[arg(long, default_value_t = 3600)]
    pub timeout: u64,
    /// SHA 指定で run がまだ現れないとき、待つ秒数(超えたら「見つからない」で終了)
    #[arg(long, default_value_t = 120)]
    pub wait_for_runs: u64,
}

#[derive(Args)]
pub struct StatusArgs {
    #[command(flatten)]
    pub target: TargetArgs,
    /// JSON で出力する
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct RunsArgs {
    /// コミット SHA(7〜40 桁)・HEAD・ブランチ名(先頭コミットを解決する)。省略時は HEAD
    pub target: Option<String>,
    /// OWNER/REPO。省略時はカレントの git remote から推定
    #[arg(long)]
    pub repo: Option<String>,
    /// この workflow の run だけを出す(表示名、または `.github/workflows/x.yml` / `x.yml`)
    #[arg(long)]
    pub workflow: Option<String>,
    /// このイベント(push / workflow_dispatch など)の run だけを出す
    #[arg(long)]
    pub event: Option<String>,
    /// この時刻(RFC 3339。例: 2026-11-18T09:00:00Z)以後に作成された run だけを出す
    #[arg(long)]
    pub since: Option<String>,
    /// 条件に合う run が現れるまで最大この秒数待つ(0 は待たず 1 回だけ取得)。
    /// 現れなければ終了コード 7
    #[arg(long, default_value_t = 0)]
    pub wait: u64,
    /// --wait 中の取得間隔(秒、5 以上)
    #[arg(long, default_value_t = 10)]
    pub interval: u64,
    /// キャッシュを新鮮とみなす秒数
    #[arg(long, default_value_t = 15)]
    pub ttl: u64,
    /// 共有キャッシュを使わず毎回取得する
    #[arg(long)]
    pub no_cache: bool,
    /// JSON 配列で出力する(作成時刻の新しい順)
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct BudgetArgs {
    /// JSON で出力する
    #[arg(long)]
    pub json: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Watch(a) => commands::watch(&a),
        Command::Status(a) => commands::status(&a),
        Command::Runs(a) => commands::runs(&a),
        Command::Budget(a) => commands::budget(&a),
    };
    match result {
        Ok(code) => ExitCode::from(code as u8),
        Err(e) => {
            eprintln!("vigilia: {e}");
            ExitCode::from(e.exit_code() as u8)
        }
    }
}
