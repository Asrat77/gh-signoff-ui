//! How long to wait between full reads, and how to tell the computer slept.
//!
//! Pure functions, so every branch is tested without a clock. The worker
//! asks [`wait`] after every read settles and reads again when it is up;
//! everything that should not wait (the popover opening, Refresh, our own
//! actions, a checkout changing) asks for a read directly instead.
//!
//! The old `refreshSeconds` setting is not read: one fixed interval was the
//! problem this replaces, and a value picked for the old 30-second loop
//! means nothing next to these. It stays in the config file so older builds
//! sharing it still find it.

use std::time::Duration;

use jiff::SignedDuration;

/// While someone is looking, or something is about to change.
pub(crate) const LIVE: Duration = Duration::from_secs(10);
/// In the tray, with pull requests open.
pub(crate) const BACKGROUND: Duration = Duration::from_secs(60);
/// Nothing open: a new pull request is all that can appear.
pub(crate) const QUIET: Duration = Duration::from_secs(5 * 60);
/// Backing off after failures never waits longer than this.
pub(crate) const MAX_BACKOFF: Duration = Duration::from_secs(5 * 60);

/// What the cadence depends on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Signals {
    /// The popover or settings window is open.
    pub(crate) visible: bool,
    /// A local verify run is in progress.
    pub(crate) running: bool,
    /// An open pull request has a check that is pending: started, and
    /// likely to finish soon.
    pub(crate) pending: bool,
    /// Any pull request is open in a tracked repository.
    pub(crate) open: bool,
}

/// How long until the next full read: [`LIVE`] while someone is looking or
/// something is in flight, [`BACKGROUND`] otherwise, [`QUIET`] with nothing
/// open; doubled for each failed read in a row, up to [`MAX_BACKOFF`] (or
/// the cadence itself, if that is longer); and never before GitHub's rate
/// limit lets reads start again, `rate_limited_for` from now.
pub(crate) fn wait(signals: Signals, failures: u32, rate_limited_for: Option<Duration>) -> Duration {
    let base = if signals.visible || signals.running || signals.pending {
        LIVE
    } else if signals.open {
        BACKGROUND
    } else {
        QUIET
    };
    let backoff = base.saturating_mul(2u32.saturating_pow(failures.min(16))).min(MAX_BACKOFF.max(base));
    backoff.max(rate_limited_for.unwrap_or_default())
}

/// Allowance on top of twice the expected wait before a late wake counts
/// as a jump: a busy machine can be a little late.
const JUMP_SLACK: Duration = Duration::from_secs(5);

/// Whether the clock jumped while the worker waited `expected`: it woke
/// far later than asked by either clock (the computer slept, or the
/// process was frozen), or the wall clock moved backwards (it was set).
/// `monotonic` and `wall` are how long the wait took by each clock; the
/// monotonic clock stops during sleep on some systems and not on others,
/// so both are checked.
pub(crate) fn clock_jumped(expected: Duration, monotonic: Duration, wall: SignedDuration) -> bool {
    let limit = expected.saturating_mul(2) + JUMP_SLACK;
    let set_back = wall.is_negative() && wall.unsigned_abs() > JUMP_SLACK;
    monotonic > limit || wall.unsigned_abs() > limit || set_back
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: fn(u64) -> Duration = Duration::from_secs;

    fn signals(visible: bool, running: bool, pending: bool, open: bool) -> Signals {
        Signals { visible, running, pending, open }
    }

    #[test]
    fn reads_often_while_looked_at_or_something_is_in_flight() {
        assert_eq!(wait(signals(true, false, false, false), 0, None), LIVE, "the popover is open");
        assert_eq!(wait(signals(false, true, false, true), 0, None), LIVE, "a local run");
        assert_eq!(wait(signals(false, false, true, true), 0, None), LIVE, "a pending check");
    }

    #[test]
    fn reads_less_in_the_background_and_least_with_nothing_open() {
        assert_eq!(wait(signals(false, false, false, true), 0, None), BACKGROUND);
        assert_eq!(wait(Signals::default(), 0, None), QUIET);
    }

    #[test]
    fn failures_back_off_exponentially_up_to_the_cap() {
        let live = signals(true, false, false, true);
        let waits: Vec<_> = (0..8).map(|failures| wait(live, failures, None)).collect();
        assert_eq!(waits, [S(10), S(20), S(40), S(80), S(160), S(300), S(300), S(300)]);
        let background = signals(false, false, false, true);
        assert_eq!(wait(background, 1, None), S(120));
        assert_eq!(wait(background, 3, None), MAX_BACKOFF);
        assert_eq!(wait(background, u32::MAX, None), MAX_BACKOFF, "no overflow");
        assert_eq!(wait(Signals::default(), 4, None), QUIET, "never shorter than the cadence");
    }

    #[test]
    fn never_reads_before_the_rate_limit_lifts() {
        let live = signals(true, false, false, true);
        assert_eq!(wait(live, 0, Some(S(900))), S(900));
        assert_eq!(wait(live, 0, Some(S(3))), LIVE, "the cadence when it lifts sooner");
        assert_eq!(wait(live, 2, Some(S(30))), S(40), "or the backoff");
        assert_eq!(wait(live, 0, Some(Duration::ZERO)), LIVE);
    }

    #[test]
    fn a_late_wake_or_a_clock_set_back_is_a_jump() {
        let wall = |secs: i64| SignedDuration::from_secs(secs);
        assert!(!clock_jumped(S(15), S(15), wall(15)), "on time");
        assert!(!clock_jumped(S(15), S(2), wall(2)), "woken early by a command");
        assert!(!clock_jumped(S(15), S(34), wall(34)), "a little late");
        assert!(clock_jumped(S(15), S(15), wall(3600)), "slept: the monotonic clock stopped");
        assert!(clock_jumped(S(15), S(3600), wall(3600)), "slept: both clocks ran on");
        assert!(clock_jumped(S(15), S(15), wall(-120)), "the clock was set back");
        assert!(!clock_jumped(S(15), S(15), wall(-1)), "a small correction");
    }
}
