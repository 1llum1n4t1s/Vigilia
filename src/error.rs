//! Vigilia 全体で使うエラー型と終了コード。
//!
//! 終了コードは呼び出し側(シェルスクリプトや AI エージェント)が結果を機械的に
//! 判別できるよう、`watch` / `status` の結論と一貫させている。

use std::time::Duration;
use thiserror::Error;

/// 終了コード定義。README にも同じ表を載せている。
pub mod exit {
    /// すべて成功(スキップ・中立は成功扱い)。
    pub const SUCCESS: i32 = 0;
    /// 1 つ以上の失敗(failure / timed_out / startup_failure / action_required)。
    pub const FAILURE: i32 = 1;
    /// 入力不正・認証・通信・API エラーなど、結論に到達できなかった。
    pub const ERROR: i32 = 2;
    /// 取消(cancelled / stale)で終了した。
    pub const CANCELLED: i32 = 3;
    /// `watch` が `--timeout` で打ち切られた。
    pub const TIMEOUT: i32 = 4;
    /// レート制限に当たり、待っても結論を得られなかった。
    pub const RATE_LIMITED: i32 = 5;
    /// `status` で対象がまだ実行中・待機中。
    pub const IN_PROGRESS: i32 = 6;
    /// 対象(リポジトリ・コミット・ブランチ・run)が見つからない、または対象に紐づく
    /// (`--workflow` / `--event` に合う)run / check suite がまだ無い。`runs` では条件に合う run が無い。
    pub const NOT_FOUND: i32 = 7;
}

#[derive(Debug, Error)]
pub enum VigiliaError {
    /// コマンドライン引数・環境(git リポジトリ外など)の問題。
    #[error("{0}")]
    Usage(String),
    /// GitHub トークンを取得できない / 拒否された。
    #[error("認証エラー: {0}")]
    Auth(String),
    /// 通信に失敗した(リトライ後も)。
    #[error("通信エラー: {0}")]
    Network(String),
    /// GitHub API がエラーを返した、または想定外のレスポンスだった。
    #[error("GitHub API エラー: {0}")]
    Api(String),
    /// 一次/二次レート制限。`retry_after` は待つべき時間(分かる場合)。
    #[error("GitHub のレート制限に当たりました: {message}")]
    RateLimited {
        retry_after: Option<Duration>,
        message: String,
    },
    /// リポジトリ・コミット・run が存在しない、または権限がない。
    #[error("{0}")]
    NotFound(String),
    /// キャッシュなどローカル I/O の失敗。
    #[error("I/O エラー: {0}")]
    Io(String),
}

impl VigiliaError {
    /// このエラーに対応する終了コード。
    pub fn exit_code(&self) -> i32 {
        match self {
            VigiliaError::RateLimited { .. } => exit::RATE_LIMITED,
            VigiliaError::NotFound(_) => exit::NOT_FOUND,
            _ => exit::ERROR,
        }
    }
}

pub type Result<T> = std::result::Result<T, VigiliaError>;
