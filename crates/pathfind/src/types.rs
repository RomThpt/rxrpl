use rxrpl_primitives::AccountId;

/// Classification of a payment by source/destination currency type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaymentType {
    XrpToXrp,
    XrpToIou,
    IouToXrp,
    IouToSameIou,
    IouToDiffIou,
}

/// An issue (currency + issuer pair).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Issue {
    pub currency: [u8; 20],
    pub issuer: AccountId,
}

impl Issue {
    pub fn xrp() -> Self {
        Self {
            currency: [0u8; 20],
            issuer: AccountId([0u8; 20]),
        }
    }

    pub fn is_xrp(&self) -> bool {
        self.currency == [0u8; 20]
    }
}

/// A single step in a payment path.
#[derive(Debug, Clone)]
pub struct PathStep {
    pub account: Option<AccountId>,
    pub currency: Option<[u8; 20]>,
    pub issuer: Option<AccountId>,
    pub step_type: u8,
}

/// Path type flags (from rippled).
pub const PATH_STEP_ACCOUNT: u8 = 0x01;
pub const PATH_STEP_CURRENCY: u8 = 0x10;
pub const PATH_STEP_ISSUER: u8 = 0x20;

/// Ranking information for a found path.
#[derive(Debug, Clone)]
pub struct PathRank {
    pub quality: f64,
    pub length: usize,
    pub liquidity: f64,
    pub index: usize,
}

/// A complete path alternative returned to the caller.
#[derive(Debug, Clone)]
pub struct PathAlternative {
    pub source_amount: serde_json::Value,
    pub paths_computed: Vec<Vec<PathStep>>,
}

/// Trust line information for pathfinding.
#[derive(Debug, Clone)]
pub struct PathFindTrustLine {
    pub peer: AccountId,
    pub currency: [u8; 20],
    pub balance: f64,
    pub limit: f64,
    pub peer_limit: f64,
    pub no_ripple: bool,
    pub peer_no_ripple: bool,
}

/// Classify a payment based on source/destination currency.
pub fn classify_payment(src_issue: &Issue, dst_issue: &Issue) -> PaymentType {
    match (src_issue.is_xrp(), dst_issue.is_xrp()) {
        (true, true) => PaymentType::XrpToXrp,
        (true, false) => PaymentType::XrpToIou,
        (false, true) => PaymentType::IouToXrp,
        (false, false) => {
            if src_issue.currency == dst_issue.currency && src_issue.issuer == dst_issue.issuer {
                PaymentType::IouToSameIou
            } else {
                PaymentType::IouToDiffIou
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_xrp_to_xrp() {
        assert_eq!(
            classify_payment(&Issue::xrp(), &Issue::xrp()),
            PaymentType::XrpToXrp
        );
    }

    #[test]
    fn classify_xrp_to_iou() {
        let mut usd = [0u8; 20];
        usd[12..15].copy_from_slice(b"USD");
        let dst = Issue {
            currency: usd,
            issuer: AccountId([1u8; 20]),
        };
        assert_eq!(classify_payment(&Issue::xrp(), &dst), PaymentType::XrpToIou);
    }

    #[test]
    fn classify_iou_to_xrp() {
        let mut usd = [0u8; 20];
        usd[12..15].copy_from_slice(b"USD");
        let src = Issue {
            currency: usd,
            issuer: AccountId([1u8; 20]),
        };
        assert_eq!(classify_payment(&src, &Issue::xrp()), PaymentType::IouToXrp);
    }

    #[test]
    fn classify_same_iou() {
        let mut usd = [0u8; 20];
        usd[12..15].copy_from_slice(b"USD");
        let issue = Issue {
            currency: usd,
            issuer: AccountId([1u8; 20]),
        };
        assert_eq!(classify_payment(&issue, &issue), PaymentType::IouToSameIou);
    }

    #[test]
    fn classify_diff_iou() {
        let mut usd = [0u8; 20];
        usd[12..15].copy_from_slice(b"USD");
        let mut eur = [0u8; 20];
        eur[12..15].copy_from_slice(b"EUR");
        let src = Issue {
            currency: usd,
            issuer: AccountId([1u8; 20]),
        };
        let dst = Issue {
            currency: eur,
            issuer: AccountId([2u8; 20]),
        };
        assert_eq!(classify_payment(&src, &dst), PaymentType::IouToDiffIou);
    }
}
