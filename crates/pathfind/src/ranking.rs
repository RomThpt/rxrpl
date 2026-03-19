use crate::types::{PathRank, PathStep};

/// Compute quality rankings for a set of found paths.
///
/// Quality is based on path length (shorter is better). In a full
/// implementation this would simulate the payment to compute actual
/// liquidity and exchange rates.
pub fn compute_path_ranks(paths: &[Vec<PathStep>]) -> Vec<PathRank> {
    paths
        .iter()
        .enumerate()
        .map(|(i, path)| {
            let length = path.len();
            // Quality: shorter paths have better (lower) quality scores
            let quality = length as f64;
            // Liquidity estimation would require simulation; use 1.0 as placeholder
            let liquidity = 1.0;

            PathRank {
                quality,
                length,
                liquidity,
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
    fn ranking_prefers_shorter_paths() {
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

        let mut ranks = compute_path_ranks(&paths);
        let best = get_best_paths(&paths, &mut ranks, 4);

        assert_eq!(best.len(), 2);
        assert_eq!(best[0].len(), 1); // shorter path first
    }

    #[test]
    fn max_limits_results() {
        let paths: Vec<Vec<PathStep>> = (0..10).map(|_| vec![]).collect();
        let mut ranks = compute_path_ranks(&paths);
        let best = get_best_paths(&paths, &mut ranks, 4);
        assert_eq!(best.len(), 4);
    }
}
