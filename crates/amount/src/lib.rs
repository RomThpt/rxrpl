/// Precise XRPL amount arithmetic.
///
/// Implements IOU amount arithmetic matching rippled's STAmount semantics,
/// using mantissa-exponent representation with 128-bit intermediate precision.
pub mod error;
pub mod iou;
pub mod quality;

pub use error::AmountError;
pub use iou::IOUAmount;
pub use quality::{from_rate, get_rate, is_better_quality, offer_quality};
