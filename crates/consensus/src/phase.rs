/// The current phase of the consensus process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsensusPhase {
    /// Collecting transactions for the next ledger.
    Open,
    /// Converging on a transaction set with other validators.
    Establish,
    /// Ledger accepted, transitioning to next round.
    Accepted,
}

impl std::fmt::Display for ConsensusPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Open => write!(f, "open"),
            Self::Establish => write!(f, "establish"),
            Self::Accepted => write!(f, "accepted"),
        }
    }
}
