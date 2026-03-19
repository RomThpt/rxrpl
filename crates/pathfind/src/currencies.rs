use std::collections::HashSet;

use rxrpl_ledger::Ledger;
use rxrpl_primitives::AccountId;

use crate::line_cache::RippleLineCache;
use crate::types::Issue;

/// Maximum number of currencies returned per account.
const MAX_CURRENCIES: usize = 88;

/// Get all currency issues available to an account via trust lines.
pub fn account_currencies(
    ledger: &Ledger,
    account: &AccountId,
    line_cache: &mut RippleLineCache,
) -> Vec<Issue> {
    let lines = line_cache.get_lines(ledger, account);

    let mut seen = HashSet::new();
    let mut issues = Vec::new();

    for line in lines {
        let issue = Issue {
            currency: line.currency,
            issuer: line.peer,
        };

        if seen.insert((issue.currency, issue.issuer)) {
            issues.push(issue);
            if issues.len() >= MAX_CURRENCIES {
                break;
            }
        }
    }

    issues
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_account_returns_empty() {
        let ledger = Ledger::genesis();
        let account = AccountId([1u8; 20]);
        let mut cache = RippleLineCache::new();
        let result = account_currencies(&ledger, &account, &mut cache);
        assert!(result.is_empty());
    }
}
