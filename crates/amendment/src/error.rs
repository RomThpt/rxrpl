/// Errors from amendment operations.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AmendmentError {
    #[error("unknown amendment: {0}")]
    UnknownAmendment(String),

    #[error("amendment already registered: {0}")]
    AlreadyRegistered(String),
}
