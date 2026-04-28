//! Freshness window for validations (rippled `Validations.h::isCurrent`).
//! Times are u32 seconds since XRPL ripple epoch (2000-01-01 UTC).

/// Maximum amount a validation's sign time may be in the future before it is
/// considered stale (mirrors rippled `validationCURRENT_WALL = 5 minutes`).
pub const VALIDATION_CURRENT_WALL: u32 = 5 * 60;
/// Maximum amount the local "seen" time may be in the future before the
/// validation is considered stale (mirrors rippled `validationCURRENT_LOCAL = 3 minutes`).
pub const VALIDATION_CURRENT_LOCAL: u32 = 3 * 60;
/// Maximum amount a validation's sign time may be in the past before it is
/// considered stale (mirrors rippled `validationCURRENT_EARLY = 3 minutes`).
pub const VALIDATION_CURRENT_EARLY: u32 = 3 * 60;

/// Returns true when the validation is "current" relative to `now`.
///
/// Mirrors rippled's `isCurrent`:
///
/// ```cpp
/// signTime > (now - validationCURRENT_EARLY) &&
/// signTime < (now + validationCURRENT_WALL) &&
/// (seenTime == NetClock::time_point{} || seenTime < (now + validationCURRENT_LOCAL))
/// ```
///
/// `seen_time = 0` is treated as "unset" (matches rippled's `NetClock::time_point{}` sentinel).
/// All arithmetic uses `saturating_sub`/`saturating_add` to avoid u32 underflow/overflow when
/// `now` is close to 0 or `u32::MAX`.
pub fn is_current(now: u32, sign_time: u32, seen_time: u32) -> bool {
    let early_floor = now.saturating_sub(VALIDATION_CURRENT_EARLY);
    let wall_ceiling = now.saturating_add(VALIDATION_CURRENT_WALL);
    let local_ceiling = now.saturating_add(VALIDATION_CURRENT_LOCAL);

    let sign_in_window = sign_time > early_floor && sign_time < wall_ceiling;
    let seen_ok = seen_time == 0 || seen_time < local_ceiling;

    sign_in_window && seen_ok
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u32 = 1_000_000;

    #[test]
    fn rejects_sign_time_too_far_in_past() {
        // EARLY = 180s. sign_time exactly at the floor or before is rejected.
        let sign_time = NOW - VALIDATION_CURRENT_EARLY - 1;
        assert!(!is_current(NOW, sign_time, 0));
    }

    #[test]
    fn rejects_sign_time_too_far_in_future() {
        // WALL = 300s. sign_time at or after the ceiling is rejected.
        let sign_time = NOW + VALIDATION_CURRENT_WALL;
        assert!(!is_current(NOW, sign_time, 0));
    }

    #[test]
    fn accepts_within_window_no_seen_time() {
        // sign_time strictly inside (now - EARLY, now + WALL); seen_time = 0 = unset.
        let sign_time = NOW + 10;
        assert!(is_current(NOW, sign_time, 0));
    }

    #[test]
    fn accepts_within_window_seen_time_within_local() {
        // sign_time inside the window, seen_time strictly less than now + LOCAL.
        let sign_time = NOW;
        let seen_time = NOW + VALIDATION_CURRENT_LOCAL - 1;
        assert!(is_current(NOW, sign_time, seen_time));
    }

    #[test]
    fn rejects_when_seen_time_too_far_ahead() {
        // sign_time inside window, but seen_time at or beyond now + LOCAL.
        let sign_time = NOW;
        let seen_time = NOW + VALIDATION_CURRENT_LOCAL;
        assert!(!is_current(NOW, sign_time, seen_time));
    }

    #[test]
    fn boundary_sign_time_equal_to_floor_is_rejected() {
        // rippled uses strict `>`: sign_time == now - EARLY must be false.
        let sign_time = NOW - VALIDATION_CURRENT_EARLY;
        assert!(!is_current(NOW, sign_time, 0));
    }

    #[test]
    fn saturating_arithmetic_when_now_is_small() {
        // now < EARLY: floor saturates to 0; any sign_time > 0 inside the wall is current.
        let now = 10;
        assert!(is_current(now, 1, 0));
        assert!(!is_current(now, 0, 0));
    }
}
