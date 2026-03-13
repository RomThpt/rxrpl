use serde::{Deserialize, Serialize};

use crate::account_id::AccountId;
use crate::currency::CurrencyCode;

/// Identifies a specific currency issued by a specific account.
/// For XRP, the issuer is typically absent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Issue {
    pub currency: CurrencyCode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issuer: Option<AccountId>,
}

impl Issue {
    pub fn xrp() -> Self {
        Self {
            currency: CurrencyCode::XRP,
            issuer: None,
        }
    }

    pub fn issued(currency: CurrencyCode, issuer: AccountId) -> Self {
        Self {
            currency,
            issuer: Some(issuer),
        }
    }

    pub fn is_xrp(&self) -> bool {
        self.currency.is_xrp()
    }
}

impl std::fmt::Display for Issue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.issuer {
            Some(issuer) => write!(f, "{}/{}", self.currency, issuer),
            None => write!(f, "{}", self.currency),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xrp_issue() {
        let issue = Issue::xrp();
        assert!(issue.is_xrp());
        assert_eq!(issue.to_string(), "XRP");
    }
}
