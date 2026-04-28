use serde::{Deserialize, Serialize};

/// XRPL API version.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApiVersion {
    #[default]
    V1 = 1,
    V2 = 2,
}
