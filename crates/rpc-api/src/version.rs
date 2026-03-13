use serde::{Deserialize, Serialize};

/// XRPL API version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApiVersion {
    V1 = 1,
    V2 = 2,
}

impl Default for ApiVersion {
    fn default() -> Self {
        Self::V1
    }
}
