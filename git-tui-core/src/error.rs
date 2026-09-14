use thiserror::Error;

/// All errors from git-tui-core. Typed here; git-tui wraps these in `anyhow`.
#[derive(Debug, Error)]
pub enum GitError {
    #[error("not inside a git repository (searched upward from {0})")]
    NotARepo(String),

    #[error(transparent)]
    Git(#[from] git2::Error),

    #[error("job channel disconnected")]
    ChannelDisconnected,

    #[error("nothing to commit: index is empty")]
    EmptyCommit,

    #[error("hunk staging failed: {0}")]
    HunkStaging(String),
}
