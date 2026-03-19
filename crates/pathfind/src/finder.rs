use rxrpl_ledger::Ledger;
use rxrpl_primitives::AccountId;

use crate::book_index::BookIndex;
use crate::line_cache::RippleLineCache;
use crate::types::{
    Issue, PATH_STEP_ACCOUNT, PATH_STEP_CURRENCY, PATH_STEP_ISSUER, PathStep, PaymentType,
    classify_payment,
};

/// Maximum number of paths considered during search.
const MAX_PATHS: usize = 1000;
/// Maximum path length (intermediate steps).
const MAX_PATH_LENGTH: usize = 6;
/// Maximum source candidates.
const MAX_SOURCE_CANDIDATES: usize = 50;
/// Maximum intermediate candidates per step.
const MAX_INTERMEDIATES: usize = 10;

/// Core DFS path finder.
pub struct Pathfinder<'a> {
    ledger: &'a Ledger,
    source: AccountId,
    destination: AccountId,
    src_issue: Issue,
    dst_issue: Issue,
    line_cache: &'a mut RippleLineCache,
    book_index: &'a BookIndex,
}

impl<'a> Pathfinder<'a> {
    pub fn new(
        ledger: &'a Ledger,
        source: AccountId,
        destination: AccountId,
        src_issue: Issue,
        dst_issue: Issue,
        line_cache: &'a mut RippleLineCache,
        book_index: &'a BookIndex,
    ) -> Self {
        Self {
            ledger,
            source,
            destination,
            src_issue,
            dst_issue,
            line_cache,
            book_index,
        }
    }

    /// Find payment paths from source to destination.
    pub fn find_paths(&mut self) -> Vec<Vec<PathStep>> {
        let payment_type = classify_payment(&self.src_issue, &self.dst_issue);

        match payment_type {
            PaymentType::XrpToXrp => {
                // Direct XRP-to-XRP: no path needed
                vec![vec![]]
            }
            PaymentType::XrpToIou => self.find_xrp_to_iou(),
            PaymentType::IouToXrp => self.find_iou_to_xrp(),
            PaymentType::IouToSameIou => self.find_iou_to_same_iou(),
            PaymentType::IouToDiffIou => self.find_iou_to_diff_iou(),
        }
    }

    /// XRP -> IOU: find accounts that have trust lines for the dst currency
    /// and also have XRP.
    fn find_xrp_to_iou(&mut self) -> Vec<Vec<PathStep>> {
        let mut paths = Vec::new();

        // Direct path: source -> dst_issuer (if they accept the IOU)
        paths.push(vec![PathStep {
            account: Some(self.dst_issue.issuer),
            currency: Some(self.dst_issue.currency),
            issuer: Some(self.dst_issue.issuer),
            step_type: PATH_STEP_ACCOUNT | PATH_STEP_CURRENCY | PATH_STEP_ISSUER,
        }]);

        // Via order books
        let book_targets = self.book_index.get_books_for(&Issue::xrp());
        for target in book_targets.iter().take(MAX_INTERMEDIATES) {
            if target.currency == self.dst_issue.currency {
                paths.push(vec![PathStep {
                    account: None,
                    currency: Some(target.currency),
                    issuer: Some(target.issuer),
                    step_type: PATH_STEP_CURRENCY | PATH_STEP_ISSUER,
                }]);
            }
        }

        paths.truncate(MAX_PATHS);
        paths
    }

    /// IOU -> XRP: find accounts that hold the source IOU and can sell for XRP.
    fn find_iou_to_xrp(&mut self) -> Vec<Vec<PathStep>> {
        let mut paths = Vec::new();

        // Direct path via issuer
        paths.push(vec![PathStep {
            account: Some(self.src_issue.issuer),
            currency: None,
            issuer: None,
            step_type: PATH_STEP_ACCOUNT,
        }]);

        // Via order books
        let book_targets = self.book_index.get_books_for(&self.src_issue);
        for target in book_targets.iter().take(MAX_INTERMEDIATES) {
            if target.is_xrp() {
                paths.push(vec![PathStep {
                    account: None,
                    currency: Some([0u8; 20]),
                    issuer: None,
                    step_type: PATH_STEP_CURRENCY,
                }]);
                break;
            }
        }

        paths.truncate(MAX_PATHS);
        paths
    }

    /// IOU -> same IOU (different issuer implied): via trust line chains.
    fn find_iou_to_same_iou(&mut self) -> Vec<Vec<PathStep>> {
        let mut paths = Vec::new();

        // Direct ripple path (no steps needed if accounts are connected)
        paths.push(vec![]);

        // Via trust lines from destination
        let dst_lines = self.line_cache.get_lines(self.ledger, &self.destination);
        for line in dst_lines.iter().take(MAX_SOURCE_CANDIDATES) {
            if line.currency == self.src_issue.currency && line.peer != self.source {
                paths.push(vec![PathStep {
                    account: Some(line.peer),
                    currency: None,
                    issuer: None,
                    step_type: PATH_STEP_ACCOUNT,
                }]);
            }
        }

        paths.truncate(MAX_PATHS);
        paths
    }

    /// IOU -> different IOU: the most complex case.
    fn find_iou_to_diff_iou(&mut self) -> Vec<Vec<PathStep>> {
        let mut paths = Vec::new();

        // Path 1: src_issue -> XRP -> dst_issue (two hops via XRP bridge)
        paths.push(vec![
            PathStep {
                account: None,
                currency: Some([0u8; 20]),
                issuer: None,
                step_type: PATH_STEP_CURRENCY,
            },
            PathStep {
                account: None,
                currency: Some(self.dst_issue.currency),
                issuer: Some(self.dst_issue.issuer),
                step_type: PATH_STEP_CURRENCY | PATH_STEP_ISSUER,
            },
        ]);

        // Path 2: direct book if available
        let book_targets = self.book_index.get_books_for(&self.src_issue);
        for target in book_targets.iter().take(MAX_INTERMEDIATES) {
            if target.currency == self.dst_issue.currency && target.issuer == self.dst_issue.issuer
            {
                paths.push(vec![PathStep {
                    account: None,
                    currency: Some(target.currency),
                    issuer: Some(target.issuer),
                    step_type: PATH_STEP_CURRENCY | PATH_STEP_ISSUER,
                }]);
            }
        }

        // Path 3: via accounts with trust lines for both currencies
        let dst_lines = self.line_cache.get_lines(self.ledger, &self.destination);
        let dst_peers: Vec<_> = dst_lines
            .iter()
            .filter(|l| l.currency == self.dst_issue.currency)
            .take(MAX_INTERMEDIATES)
            .map(|l| l.peer)
            .collect();

        for peer in &dst_peers {
            if *peer == self.source || *peer == self.destination {
                continue;
            }

            let peer_lines = self.line_cache.get_lines(self.ledger, peer);
            let has_src = peer_lines
                .iter()
                .any(|l| l.currency == self.src_issue.currency);

            if has_src && paths.len() < MAX_PATH_LENGTH {
                paths.push(vec![
                    PathStep {
                        account: Some(*peer),
                        currency: None,
                        issuer: None,
                        step_type: PATH_STEP_ACCOUNT,
                    },
                    PathStep {
                        account: None,
                        currency: Some(self.dst_issue.currency),
                        issuer: Some(self.dst_issue.issuer),
                        step_type: PATH_STEP_CURRENCY | PATH_STEP_ISSUER,
                    },
                ]);
            }
        }

        paths.truncate(MAX_PATHS);
        paths
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xrp_to_xrp_returns_empty_path() {
        let ledger = Ledger::genesis();
        let src = AccountId([1u8; 20]);
        let dst = AccountId([2u8; 20]);
        let book_index = BookIndex::build(&ledger);
        let mut line_cache = RippleLineCache::new();

        let mut finder = Pathfinder::new(
            &ledger,
            src,
            dst,
            Issue::xrp(),
            Issue::xrp(),
            &mut line_cache,
            &book_index,
        );

        let paths = finder.find_paths();
        assert_eq!(paths.len(), 1);
        assert!(paths[0].is_empty());
    }
}
