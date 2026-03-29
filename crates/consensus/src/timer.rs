use std::time::{Duration, Instant};

use crate::params::ConsensusParams;
use crate::phase::ConsensusPhase;

/// Actions the consensus timer requests the caller to perform.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TimerAction {
    /// Close the open ledger and begin establishing consensus.
    CloseLedger,
    /// Run a convergence round (call `converge()` on the engine).
    Converge,
    /// Consensus has stalled -- abort the current round and start fresh.
    StallAbort,
}

/// Timer-driven tick mechanism for the consensus engine.
///
/// Encapsulates the timing logic from rippled's `timerEntry()`:
/// - Open phase: wait for the close interval, then request close
/// - Establish phase: periodically request convergence rounds
/// - Stall detection: abort after a configurable timeout
/// - Adaptive behavior: shorten open duration when previous rounds
///   converged quickly
///
/// The timer does not own the consensus engine -- it returns actions
/// that the caller executes. This keeps it testable without async.
pub struct ConsensusTimer {
    /// When the current phase started.
    phase_start: Instant,
    /// Current consensus phase (mirrored from the engine).
    phase: ConsensusPhase,
    /// How long to keep the ledger open before closing.
    open_duration: Duration,
    /// How often to attempt convergence during Establish.
    converge_interval: Duration,
    /// When we last ran a convergence tick.
    last_converge: Instant,
    /// Maximum time in Establish before declaring a stall.
    stall_timeout: Duration,
    /// Duration of the most recent successful Establish phase.
    /// Used for adaptive open duration.
    last_converge_duration: Option<Duration>,
    /// Minimum open duration (floor for adaptive shortening).
    min_open_duration: Duration,
    /// Base open duration from params (ceiling for adaptive).
    base_open_duration: Duration,
}

impl ConsensusTimer {
    /// Create a new timer from consensus params.
    pub fn new(params: &ConsensusParams) -> Self {
        let open_duration = Duration::from_millis(params.ledger_idle_interval_ms);
        let converge_interval = Duration::from_millis(params.propose_interval_ms);
        // Stall timeout: max_consensus_rounds * converge_interval * 2
        // Gives generous headroom beyond what the engine itself would force-accept at.
        let stall_timeout = converge_interval
            .checked_mul(params.max_consensus_rounds * 2)
            .unwrap_or(Duration::from_secs(60));

        let now = Instant::now();
        Self {
            phase_start: now,
            phase: ConsensusPhase::Open,
            open_duration,
            converge_interval,
            last_converge: now,
            stall_timeout,
            last_converge_duration: None,
            min_open_duration: Duration::from_millis(2_000),
            base_open_duration: open_duration,
        }
    }

    /// Get the current phase tracked by the timer.
    pub fn phase(&self) -> ConsensusPhase {
        self.phase
    }

    /// Get the current open duration (may be adapted).
    pub fn open_duration(&self) -> Duration {
        self.open_duration
    }

    /// Get the stall timeout.
    pub fn stall_timeout(&self) -> Duration {
        self.stall_timeout
    }

    /// Notify the timer that the phase has changed.
    ///
    /// Must be called after `close_ledger()`, `converge()` returns true,
    /// or `start_round()` on the engine.
    pub fn on_phase_change(&mut self, new_phase: ConsensusPhase) {
        let now = Instant::now();

        // Track how long Establish took for adaptive timing
        if self.phase == ConsensusPhase::Establish && new_phase == ConsensusPhase::Accepted {
            self.last_converge_duration = Some(now.duration_since(self.phase_start));
        }

        // When transitioning to Open, adapt the open duration based on
        // how quickly the previous round converged.
        if new_phase == ConsensusPhase::Open {
            self.adapt_open_duration();
        }

        self.phase = new_phase;
        self.phase_start = now;
        self.last_converge = now;
    }

    /// Check what action (if any) should be taken at this instant.
    ///
    /// Returns `None` if no action is needed yet.
    pub fn tick(&mut self) -> Option<TimerAction> {
        let now = Instant::now();
        let elapsed = now.duration_since(self.phase_start);

        match self.phase {
            ConsensusPhase::Open => {
                if elapsed >= self.open_duration {
                    Some(TimerAction::CloseLedger)
                } else {
                    None
                }
            }
            ConsensusPhase::Establish => {
                // Stall detection first
                if elapsed >= self.stall_timeout {
                    return Some(TimerAction::StallAbort);
                }
                // Periodic convergence
                let since_last = now.duration_since(self.last_converge);
                if since_last >= self.converge_interval {
                    self.last_converge = now;
                    Some(TimerAction::Converge)
                } else {
                    None
                }
            }
            ConsensusPhase::Accepted => {
                // Nothing to do -- caller should start a new round
                None
            }
        }
    }

    /// How long until the next meaningful event. Useful for setting a
    /// sleep/poll interval. Returns `None` in Accepted phase.
    pub fn time_until_next_action(&self) -> Option<Duration> {
        let now = Instant::now();
        let elapsed = now.duration_since(self.phase_start);

        match self.phase {
            ConsensusPhase::Open => {
                Some(self.open_duration.saturating_sub(elapsed))
            }
            ConsensusPhase::Establish => {
                let since_last = now.duration_since(self.last_converge);
                Some(self.converge_interval.saturating_sub(since_last))
            }
            ConsensusPhase::Accepted => None,
        }
    }

    /// Adapt the open duration based on how quickly convergence happened.
    ///
    /// If the last round converged in less than half the converge_interval,
    /// reduce the open duration (down to min_open_duration). If it took
    /// longer, restore toward the base duration.
    fn adapt_open_duration(&mut self) {
        let converge_dur = match self.last_converge_duration {
            Some(d) => d,
            None => return,
        };

        // "Fast" means convergence happened within one converge_interval
        let fast_threshold = self.converge_interval;

        if converge_dur <= fast_threshold {
            // Convergence was fast -- reduce open time by 20% toward minimum
            let reduction = self.open_duration / 5;
            self.open_duration = self
                .open_duration
                .saturating_sub(reduction)
                .max(self.min_open_duration);
        } else {
            // Convergence was slower -- restore open time by 10% toward base
            let increase = self.base_open_duration / 10;
            self.open_duration = (self.open_duration + increase).min(self.base_open_duration);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn default_params() -> ConsensusParams {
        ConsensusParams::default()
    }

    fn fast_params() -> ConsensusParams {
        ConsensusParams {
            ledger_idle_interval_ms: 100,
            propose_interval_ms: 50,
            max_consensus_rounds: 5,
            ..ConsensusParams::default()
        }
    }

    fn adaptive_params() -> ConsensusParams {
        ConsensusParams {
            ledger_idle_interval_ms: 5_000,
            propose_interval_ms: 50,
            max_consensus_rounds: 5,
            ..ConsensusParams::default()
        }
    }

    #[test]
    fn initial_state_is_open() {
        let timer = ConsensusTimer::new(&default_params());
        assert_eq!(timer.phase(), ConsensusPhase::Open);
    }

    #[test]
    fn tick_returns_close_after_open_duration() {
        let params = fast_params();
        let mut timer = ConsensusTimer::new(&params);

        // Immediately: no action
        assert_eq!(timer.tick(), None);

        // Wait for open duration
        thread::sleep(Duration::from_millis(110));
        assert_eq!(timer.tick(), Some(TimerAction::CloseLedger));
    }

    #[test]
    fn tick_returns_converge_during_establish() {
        let params = fast_params();
        let mut timer = ConsensusTimer::new(&params);

        // Transition to Establish
        timer.on_phase_change(ConsensusPhase::Establish);
        assert_eq!(timer.phase(), ConsensusPhase::Establish);

        // Wait for converge interval
        thread::sleep(Duration::from_millis(60));
        assert_eq!(timer.tick(), Some(TimerAction::Converge));
    }

    #[test]
    fn stall_detection_fires() {
        let params = ConsensusParams {
            ledger_idle_interval_ms: 50,
            propose_interval_ms: 20,
            max_consensus_rounds: 2,
            ..ConsensusParams::default()
        };
        let mut timer = ConsensusTimer::new(&params);

        timer.on_phase_change(ConsensusPhase::Establish);

        // Stall timeout = 20ms * 2 * 2 = 80ms
        thread::sleep(Duration::from_millis(90));
        assert_eq!(timer.tick(), Some(TimerAction::StallAbort));
    }

    #[test]
    fn accepted_phase_returns_none() {
        let params = fast_params();
        let mut timer = ConsensusTimer::new(&params);

        timer.on_phase_change(ConsensusPhase::Accepted);
        assert_eq!(timer.tick(), None);
        assert_eq!(timer.time_until_next_action(), None);
    }

    #[test]
    fn phase_change_resets_timers() {
        let params = fast_params();
        let mut timer = ConsensusTimer::new(&params);

        thread::sleep(Duration::from_millis(60));
        timer.on_phase_change(ConsensusPhase::Establish);

        // Should not immediately fire converge (just transitioned)
        assert_eq!(timer.tick(), None);
    }

    #[test]
    fn adaptive_shortens_on_fast_convergence() {
        let params = adaptive_params();
        let mut timer = ConsensusTimer::new(&params);
        let initial_open = timer.open_duration();

        // Simulate a fast consensus round
        timer.on_phase_change(ConsensusPhase::Establish);
        // Converge quickly (within converge_interval of 50ms)
        thread::sleep(Duration::from_millis(10));
        timer.on_phase_change(ConsensusPhase::Accepted);

        // Start new round -> triggers adaptation
        timer.on_phase_change(ConsensusPhase::Open);

        assert!(
            timer.open_duration() < initial_open,
            "open duration should decrease after fast convergence: {:?} vs {:?}",
            timer.open_duration(),
            initial_open,
        );
    }

    #[test]
    fn adaptive_restores_on_slow_convergence() {
        let params = adaptive_params();
        let mut timer = ConsensusTimer::new(&params);

        // First: fast convergence to shorten the open duration
        timer.on_phase_change(ConsensusPhase::Establish);
        thread::sleep(Duration::from_millis(5));
        timer.on_phase_change(ConsensusPhase::Accepted);
        timer.on_phase_change(ConsensusPhase::Open);

        let shortened = timer.open_duration();

        // Second: slow convergence (longer than converge_interval of 50ms)
        timer.on_phase_change(ConsensusPhase::Establish);
        thread::sleep(Duration::from_millis(60));
        timer.on_phase_change(ConsensusPhase::Accepted);
        timer.on_phase_change(ConsensusPhase::Open);

        assert!(
            timer.open_duration() > shortened,
            "open duration should increase after slow convergence: {:?} vs {:?}",
            timer.open_duration(),
            shortened,
        );
    }

    #[test]
    fn adaptive_respects_minimum() {
        let params = adaptive_params();
        let mut timer = ConsensusTimer::new(&params);

        // Run many fast rounds to drive the open duration down
        for _ in 0..50 {
            timer.on_phase_change(ConsensusPhase::Establish);
            timer.on_phase_change(ConsensusPhase::Accepted);
            timer.on_phase_change(ConsensusPhase::Open);
        }

        assert!(
            timer.open_duration() >= Duration::from_millis(2_000),
            "open duration should not drop below minimum: {:?}",
            timer.open_duration(),
        );
    }

    #[test]
    fn time_until_next_action_decreases() {
        let params = fast_params();
        let timer = ConsensusTimer::new(&params);

        let t1 = timer.time_until_next_action().unwrap();
        thread::sleep(Duration::from_millis(20));
        let t2 = timer.time_until_next_action().unwrap();

        assert!(t2 < t1, "time_until_next should decrease: {:?} vs {:?}", t2, t1);
    }

    #[test]
    fn converge_interval_respected() {
        let params = fast_params();
        let mut timer = ConsensusTimer::new(&params);

        timer.on_phase_change(ConsensusPhase::Establish);

        // First converge fires after interval
        thread::sleep(Duration::from_millis(55));
        assert_eq!(timer.tick(), Some(TimerAction::Converge));

        // Immediately after, should not fire again
        assert_eq!(timer.tick(), None);

        // Wait another interval
        thread::sleep(Duration::from_millis(55));
        assert_eq!(timer.tick(), Some(TimerAction::Converge));
    }
}
