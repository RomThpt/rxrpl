/// Errors from amendment operations.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AmendmentError {
    #[error("unknown amendment: {0}")]
    UnknownAmendment(String),

    #[error("amendment already registered: {0}")]
    AlreadyRegistered(String),

    #[error("unknown amendment preset: {0}")]
    UnknownPreset(String),

    #[error("amendment listed in both vote and veto: {0}")]
    DuplicateAmendment(String),

    #[error("amendments.compatibility cannot be combined with amendments.vote or amendments.veto")]
    ConfigConflict,
}
