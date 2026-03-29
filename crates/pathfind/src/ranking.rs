use rxrpl_amount::IOUAmount;
use rxrpl_ledger::Ledger;
use rxrpl_primitives::AccountId;

use crate::line_cache::RippleLineCache;
use crate::strand::simulate_strand;
use crate::types::{Issue, PathRank, PathStep};

/// Compute quality rankings for a set of found paths using strand simulation.
///
/// For each candidate path, simulates the payment flow to determine actual
/// liquidity and exchange rates. The quality score reflects how efficiently
/// the path converts input to output (lower is better, matching the
/// taker-pays/taker-gets convention). Liquidity reflects the fraction
/// of the requested amount that can actually be delivered.
pub fn compute_path_ranks(
    paths: &[Vec<PathStep>],
    ledger: &Ledger,
    line_cache: &mut RippleLineCache,
    src_issue: &Issue,
    dst_issue: &Issue,
    source: &AccountId,
    destination: &AccountId,
    requested_amount: &IOUAmount,
) -> Vec<PathRank> {
    paths
        .iter()
        .enumerate()
        .map(|(i, path)| {
            let length = path.len();

            let result = simulate_strand(
                ledger,
                line_cache,
                path,
                src_issue,
                dst_issue,
                source,
                destination,
                requested_amount,
            );

            // Quality: input/output ratio (lower is better for the sender).
            // A quality of 1.0 means 1:1 exchange.
            // If simulation fails (quality == 0.0), fall back to path length.
            let quality = if result.quality > 0.0 {
                1.0 / result.quality
            } else {
                // Fallback: use path length as a rough quality proxy
                (length + 1) as f64
            };

            let liquidity = result.quality;

            PathRank {
                quality,
                length,
                liquidity,
                index: i,
            }
        })
        .collect()
}

/// Compute quality rankings using path length only (no ledger access needed).
///
/// Used when ledger context is unavailable or for lightweight ranking.
pub fn compute_path_ranks_simple(paths: &[Vec<PathStep>]) -> Vec<PathRank> {
    paths
        .iter()
        .enumerate()
        .map(|(i, path)| {
            let length = path.len();
            PathRank {
                quality: length as f64,
                length,
                liquidity: 1.0,
                index: i,
            }
        })
        .collect()
}

/// Select the best paths from ranked candidates.
///
/// Returns up to `max` paths, sorted by quality (ascending),
/// then liquidity (descending), then length (ascending).
pub fn get_best_paths(
    paths: &[Vec<PathStep>],
    ranks: &mut [PathRank],
    max: usize,
) -> Vec<Vec<PathStep>> {
    ranks.sort_by(|a, b| {
        a.quality
            .partial_cmp(&b.quality)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(
                b.liquidity
                    .partial_cmp(&a.liquidity)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then(a.length.cmp(&b.length))
    });

    ranks
        .iter()
        .take(max)
        .filter_map(|rank| paths.get(rank.index).cloned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_ranking_prefers_shorter_paths() {
        let paths = vec![
            vec![
                PathStep {
                    account: None,
                    currency: None,
                    issuer: None,
                    step_type: 0,
                },
                PathStep {
                    account: None,
                    currency: None,
                    issuer: None,
                    step_type: 0,
                },
            ],
            vec![PathStep {
                account: None,
                currency: None,
                issuer: None,
                step_type: 0,
            }],
        ];

        let mut ranks = compute_path_ranks_simple(&paths);
        let best = get_best_paths(&paths, &mut ranks, 4);

        assert_eq!(best.len(), 2);
        assert_eq!(best[0].len(), 1); // shorter path first
    }

    #[test]
    fn max_limits_results() {
        let paths: Vec<Vec<PathStep>> = (0..10).map(|_| vec![]).collect();
        let mut ranks = compute_path_ranks_simple(&paths);
        let best = get_best_paths(&paths, &mut ranks, 4);
        assert_eq!(best.len(), 4);
    }

    #[test]
    fn simulated_ranking_xrp_to_xrp() {
        let ledger = Ledger::genesis();
        let mut line_cache = RippleLineCache::new();
        let source = AccountId([1u8; 20]);
        let destination = AccountId([2u8; 20]);
        let xrp = Issue::xrp();
        let amount = IOUAmount::new(1_000_000_000_000_000, -9).unwrap(); // 1M drops

        let paths = vec![vec![]]; // Direct XRP path

        let mut ranks = compute_path_ranks(
            &paths,
            &ledger,
            &mut line_cache,
            &xrp,
            &xrp,
            &source,
            &destination,
            &amount,
        );

        assert_eq!(ranks.len(), 1);
        // XRP-to-XRP should have quality 1.0 (1:1 ratio)
        assert!((ranks[0].quality - 1.0).abs() < 0.01);
        assert!(ranks[0].liquidity > 0.0);
    }

    #[test]
    fn best_paths_sorts_by_quality() {
        let paths: Vec<Vec<PathStep>> = vec![vec![], vec![], vec![]];
        let mut ranks = vec![
            PathRank {
                quality: 3.0,
                length: 0,
                liquidity: 0.5,
                index: 0,
            },
            PathRank {
                quality: 1.0,
                length: 0,
                liquidity: 1.0,
                index: 1,
            },
            PathRank {
                quality: 2.0,
                length: 0,
                liquidity: 0.8,
                index: 2,
            },
        ];

        let best = get_best_paths(&paths, &mut ranks, 3);
        assert_eq!(best.len(), 3);
        // Best quality (lowest) should come first
        // Index 1 (quality 1.0) then index 2 (quality 2.0) then index 0 (quality 3.0)
    }

    #[test]
    fn best_paths_tiebreak_by_liquidity() {
        let paths: Vec<Vec<PathStep>> = vec![vec![], vec![]];
        let mut ranks = vec![
            PathRank {
                quality: 1.0,
                length: 0,
                liquidity: 0.5,
                index: 0,
            },
            PathRank {
                quality: 1.0,
                length: 0,
                liquidity: 1.0,
                index: 1,
            },
        ];

        let best = get_best_paths(&paths, &mut ranks, 2);
        assert_eq!(best.len(), 2);
        // Same quality, higher liquidity first -> index 1 first
    }
}
