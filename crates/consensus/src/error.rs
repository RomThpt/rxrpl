/// Errors from consensus operations.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConsensusError {
    #[error("wrong phase: expected {expected}, got {actual}")]
    WrongPhase { expected: String, actual: String },

    #[error("invalid proposal")]
    InvalidProposal,

    #[error("invalid validation")]
    InvalidValidation,
}
