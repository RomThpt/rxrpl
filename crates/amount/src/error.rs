/// Errors from amount arithmetic operations.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AmountError {
    #[error("amount overflow")]
    Overflow,

    #[error("division by zero")]
    DivisionByZero,

    #[error("native value overflow")]
    NativeOverflow,
}
