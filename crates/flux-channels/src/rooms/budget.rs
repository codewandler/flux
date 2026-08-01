//! [`ReplyBudget`] — the hard ceiling on how often one room can make flux answer (D-207).
//!
//! ## Why a ceiling and not a heuristic
//!
//! Two flux agents in one room, each answering the other's mention, is an exchange with no natural
//! end, started by one human sentence, billed per turn. The [address rule](super::AddressRule)
//! refuses another *declared* agent's plain text — but XMPP presence carries no human-or-bot signal,
//! so a real MUC reports [`OccupantKind::Unknown`](super::OccupantKind::Unknown) for everyone
//! (D-205) and that arm never fires there. Something has to bound the case flux cannot see, and it
//! has to bound it **by construction**: at most `max` turns per `window`, counted, with no shape of
//! conversation that gets more.
//!
//! ## What it gates, and what happens when it is spent
//!
//! It gates the **turn**, not the outbound line. A silent-but-thinking agent still burns spend, so
//! deciding after the planner ran would defeat the purpose.
//!
//! A message that arrives with the budget spent is **dropped silently** — it is still overheard, so
//! it reaches the context, but nothing is said. Announcing the exhaustion would be a reply, and two
//! agents announcing it at each other is the same runaway one layer up.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How many turns one room may make flux take per [`DEFAULT_ROOM_REPLY_WINDOW`]. Enough for a real
/// back-and-forth in a meeting; far below the point where a runaway exchange costs real money.
pub const DEFAULT_ROOM_REPLY_BUDGET: usize = 12;

/// The window [`DEFAULT_ROOM_REPLY_BUDGET`] is counted over.
pub const DEFAULT_ROOM_REPLY_WINDOW: Duration = Duration::from_secs(60);

/// The most slots [`ReplyBudget::new`] will preallocate, whatever ceiling the operator configured.
const PREALLOC_CAP: usize = 64;

/// A per-room sliding-window ceiling on answered turns.
///
/// One budget belongs to one [`RoomTurnDriver`](super::RoomTurnDriver), which is what makes it
/// per-room without any room ever being named: a driver drives exactly one room for its whole life.
#[derive(Debug)]
pub struct ReplyBudget {
    max: usize,
    window: Duration,
    /// When each of the last `max` turns was granted, oldest first.
    granted: Mutex<VecDeque<Instant>>,
}

impl Default for ReplyBudget {
    fn default() -> Self {
        Self::new(DEFAULT_ROOM_REPLY_BUDGET, DEFAULT_ROOM_REPLY_WINDOW)
    }
}

impl ReplyBudget {
    /// At most `max` turns per `window`. A `max` of zero is honoured literally — a room flux listens
    /// to and never answers is a legitimate (if unusual) configuration, and quietly turning it into
    /// one is not.
    pub fn new(max: usize, window: Duration) -> Self {
        Self {
            max,
            window,
            // Capped, because `max` comes from operator configuration: `with_capacity(usize::MAX)`
            // is a `capacity overflow` abort, and a channel declaration must never be able to kill
            // the process. Capacity is only an allocation hint — the deque still grows to whatever
            // the ceiling really is, one turn at a time, and a budget that large is "effectively
            // unlimited", which is a thing an operator is allowed to ask for.
            granted: Mutex::new(VecDeque::with_capacity(max.min(PREALLOC_CAP))),
        }
    }

    /// The ceiling.
    pub fn max(&self) -> usize {
        self.max
    }

    /// The window the ceiling is counted over.
    pub fn window(&self) -> Duration {
        self.window
    }

    /// Take one turn's worth of budget at `now`, or refuse. Refusing costs nothing and changes
    /// nothing — a refused message never consumes budget, so a room that goes quiet recovers on the
    /// window alone.
    pub fn try_take(&self, now: Instant) -> bool {
        let mut granted = self.granted.lock().unwrap();
        while granted
            .front()
            .is_some_and(|t| now.duration_since(*t) >= self.window)
        {
            granted.pop_front();
        }
        if granted.len() >= self.max {
            return false;
        }
        granted.push_back(now);
        true
    }

    /// How many turns are still available at `now`, without taking one.
    pub fn remaining(&self, now: Instant) -> usize {
        let granted = self.granted.lock().unwrap();
        let live = granted
            .iter()
            .filter(|t| now.duration_since(**t) < self.window)
            .count();
        self.max.saturating_sub(live)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ceiling_holds_however_fast_the_room_talks() {
        let budget = ReplyBudget::new(3, Duration::from_secs(60));
        let now = Instant::now();
        assert_eq!(budget.remaining(now), 3);
        for i in 0..3 {
            assert!(budget.try_take(now), "turn {i} is within budget");
        }
        for i in 0..1_000 {
            assert!(
                !budget.try_take(now),
                "turn {i} past the ceiling is refused"
            );
        }
        assert_eq!(budget.remaining(now), 0);
    }

    #[test]
    fn the_window_slides_rather_than_resetting_on_a_boundary() {
        let budget = ReplyBudget::new(2, Duration::from_secs(10));
        let t0 = Instant::now();
        assert!(budget.try_take(t0));
        assert!(budget.try_take(t0 + Duration::from_secs(5)));
        assert!(!budget.try_take(t0 + Duration::from_secs(9)));
        // The first grant has aged out; the second has not.
        assert!(budget.try_take(t0 + Duration::from_secs(11)));
        assert!(!budget.try_take(t0 + Duration::from_secs(12)));
        assert_eq!(budget.remaining(t0 + Duration::from_secs(30)), 2);
    }

    #[test]
    fn a_refused_turn_does_not_consume_budget() {
        // Otherwise a runaway exchange would keep pushing the recovery point out and the room would
        // never speak again — a rate limit that punishes the room for the runaway's persistence.
        let budget = ReplyBudget::new(1, Duration::from_secs(10));
        let t0 = Instant::now();
        assert!(budget.try_take(t0));
        for i in 1..10 {
            assert!(!budget.try_take(t0 + Duration::from_secs(i)));
        }
        assert!(
            budget.try_take(t0 + Duration::from_secs(10)),
            "the single grant aged out on schedule"
        );
    }

    #[test]
    fn a_zero_budget_answers_nothing() {
        let budget = ReplyBudget::new(0, Duration::from_secs(60));
        assert!(!budget.try_take(Instant::now()));
        assert_eq!(budget.remaining(Instant::now()), 0);
    }

    #[test]
    fn an_absurd_configured_ceiling_does_not_abort_the_process() {
        // `max` arrives from the `reply_budget` channel setting, so `VecDeque::with_capacity(max)`
        // put a `capacity overflow` abort behind a number in a declaration file. A ceiling nobody
        // will reach still has to *work*, not take the host down at construction.
        let budget = ReplyBudget::new(usize::MAX, Duration::from_secs(60));
        assert!(budget.try_take(Instant::now()));
        assert_eq!(budget.max(), usize::MAX);
    }

    #[test]
    fn the_default_is_the_documented_pair() {
        let budget = ReplyBudget::default();
        assert_eq!(budget.max(), DEFAULT_ROOM_REPLY_BUDGET);
        assert_eq!(budget.window(), DEFAULT_ROOM_REPLY_WINDOW);
    }
}
