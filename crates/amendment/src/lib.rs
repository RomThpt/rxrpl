/// XRPL amendment feature registry, voting, and rules.
///
/// Provides:
/// - `Feature`: Amendment definition with SHA-512-Half-derived ID
/// - `FeatureRegistry`: Static registry of all known amendments
/// - `AmendmentTable`: Runtime voting and activation tracking
/// - `Rules`: Immutable snapshot of enabled amendments for a ledger
pub mod error;
pub mod feature;
pub mod registry;
pub mod rules;
pub mod table;

pub use error::AmendmentError;
pub use feature::{Feature, feature_id};
pub use registry::FeatureRegistry;
pub use rules::Rules;
pub use table::AmendmentTable;
