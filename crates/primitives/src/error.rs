use thiserror::Error;

#[derive(Debug, Error)]
pub enum PrimitivesError {
    #[error("invalid hex string: {0}")]
    InvalidHex(String),

    #[error("invalid length: expected {expected}, got {got}")]
    InvalidLength { expected: usize, got: usize },

    #[error("invalid amount: {0}")]
    InvalidAmount(String),

    #[error("invalid currency code: {0}")]
    InvalidCurrency(String),

    #[error("invalid account id: {0}")]
    InvalidAccountId(String),

    #[error("overflow in amount arithmetic")]
    Overflow,
}
