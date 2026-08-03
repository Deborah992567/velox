//! Hierarchical hashed timer wheel.
//!
//! Scheduling a timer is O(1): the entry is hashed into a slot based on its
//! deadline. Cancellation is O(1): the entry is removed from the bookkeeping
//! map; the stale slot entry is skipped when the slot is finally reached.
//!
//! The wheel has two levels (see ADR 0002 and `docs/architecture.md` §3.1):
//! a fine-grained level 0 (`tick0` × 256 slots) and a coarse level 1
//! (`tick0 × 256` × 256 slots). Deadlines beyond the level-1 span are parked
//! in an overflow list that is drained back into the wheel as time advances.
//! Expiry latency is bounded by the tick resolution, which is the accepted
//! trade-off of a hashed wheel.
//!
//! Time is injected: the reactor drives [`TimerWheel::poll`] with the current
//! [`Instant`], which keeps the wheel deterministic and testable.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use crate::platform::Token;

/// The connection/protocol stage a timer belongs to.
///
/// The wheel treats this as opaque metadata; the reactor uses it to pick a
/// timeout handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeoutKind {
    /// Waiting to complete an accepted connection handshake.
    Accept,
    /// Waiting for the request line and headers.
    HeadRead,
    /// Waiting for the request body.
    BodyRead,
    /// Keep-alive idle between requests.
    Idle,
    /// Writing the response.
    Send,
    /// Graceful-close deadline after an error.
    Shutdown,
    /// Establishing an upstream connection.
    ProxyConnect,
    /// Reading the upstream response.
    UpstreamRead,
    /// Writing the upstream request body.
    UpstreamWrite,
    /// Probing an upstream for health.
    HealthCheck,
}

impl TimeoutKind {
    /// A stable, lowercase name for logs and metrics.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::HeadRead => "head_read",
            Self::BodyRead => "body_read",
            Self::Idle => "idle",
            Self::Send => "send",
            Self::Shutdown => "shutdown",
            Self::ProxyConnect => "proxy_connect",
            Self::UpstreamRead => "upstream_read",
            Self::UpstreamWrite => "upstream_write",
            Self::HealthCheck => "health_check",
        }
    }
}

/// An opaque handle to a scheduled timer, used to cancel it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimerId(u64);

/// A timer that has expired, delivered by [`TimerWheel::poll`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimerEvent {
    /// The handle of the expired timer.
    pub id: TimerId,
    /// The connection token the timer was scheduled for.
    pub token: Token,
    /// The stage this timer belonged to.
    pub kind: TimeoutKind,
}

/// One scheduled timer as stored by the wheel.
#[derive(Debug, Clone, Copy)]
struct TimerEntry {
    id: TimerId,
    token: Token,
    deadline: Instant,
    kind: TimeoutKind,
}

/// Number of slots per wheel level.
const SLOTS_PER_LEVEL: usize = 256;

/// A hierarchical hashed timing wheel.
///
/// Not `Send`-safe by design: one wheel belongs to one reactor/thread.
#[derive(Debug)]
pub struct TimerWheel {
    tick: [Duration; 2],
    span: [Duration; 3],
    current: [usize; 2],
    base: Instant,
    wheel: [Vec<VecDeque<TimerId>>; 2],
    overflow: Vec<TimerEntry>,
    timers: HashMap<TimerId, TimerEntry>,
    next_id: u64,
}

impl TimerWheel {
    /// Create a wheel with a 10 ms tick and 256 slots per level.
    pub fn new() -> Self {
        Self::with_resolution(Duration::from_millis(10), SLOTS_PER_LEVEL)
    }

    /// Create a wheel with the given base tick and slot count per level.
    ///
    /// # Panics
    ///
    /// Panics if `resolution` is zero or `slots_per_level` is zero.
    pub fn with_resolution(resolution: Duration, slots_per_level: usize) -> Self {
        assert!(
            !resolution.is_zero(),
            "timer wheel resolution must be non-zero"
        );
        assert!(
            slots_per_level > 0,
            "timer wheel needs at least one slot per level"
        );
        let slots = u32::try_from(slots_per_level).expect("slot count fits u32");
        let tick0 = resolution;
        let tick1 = tick0.saturating_mul(slots);
        let tick2 = tick1.saturating_mul(slots);
        Self {
            tick: [tick0, tick1],
            span: [tick0, tick1, tick2],
            current: [0, 0],
            base: Instant::now(),
            wheel: [
                vec![VecDeque::new(); slots_per_level],
                vec![VecDeque::new(); slots_per_level],
            ],
            overflow: Vec::new(),
            timers: HashMap::new(),
            next_id: 0,
        }
    }

    /// Schedule a timer for `token` that expires at `deadline`.
    pub fn insert(&mut self, token: Token, deadline: Instant, kind: TimeoutKind) -> TimerId {
        let id = self.allocate_id();
        let entry = TimerEntry {
            id,
            token,
            deadline,
            kind,
        };
        self.timers.insert(id, entry);
        let delta = deadline.saturating_duration_since(self.base);
        if delta < self.span[2] {
            self.place(id, delta);
        } else {
            self.overflow.push(entry);
        }
        id
    }

    /// Cancel a scheduled timer. Returns `false` if it already fired or was
    /// never scheduled.
    pub fn cancel(&mut self, id: TimerId) -> bool {
        self.timers.remove(&id).is_some()
    }

    /// Collect and return every timer expired by `now`.
    ///
    /// Expired entries are removed; pending entries are left in place.
    pub fn poll(&mut self, now: Instant) -> Vec<TimerEvent> {
        let mut due = Vec::new();
        self.sweep_overflow(now, &mut due);
        loop {
            // Fire whatever is due in the current window first; only then
            // advance, so a timer placed in the current slot can never be
            // stranded when its deadline passes mid-poll.
            self.collect_current(now, &mut due);
            if self
                .base
                .checked_add(self.tick[0])
                .is_none_or(|boundary| boundary > now)
            {
                break;
            }
            self.advance();
        }
        due
    }

    /// The next instant the reactor must wake up at, if any timer is pending.
    pub fn next_deadline(&self) -> Option<Instant> {
        if self.timers.is_empty() {
            return None;
        }
        self.base.checked_add(self.tick[0])
    }

    /// Number of pending timers.
    pub fn len(&self) -> usize {
        self.timers.len()
    }

    /// Whether any timer is pending.
    pub fn is_empty(&self) -> bool {
        self.timers.is_empty()
    }

    fn allocate_id(&mut self) -> TimerId {
        loop {
            self.next_id = self.next_id.wrapping_add(1);
            let id = TimerId(self.next_id);
            if !self.timers.contains_key(&id) {
                return id;
            }
        }
    }

    fn place(&mut self, id: TimerId, delta: Duration) {
        let (level, tick) = if delta < self.span[1] {
            (0usize, self.tick[0])
        } else {
            (1usize, self.tick[1])
        };
        let ratio =
            usize::try_from(delta.as_nanos() / tick.as_nanos()).expect("deadline ratio fits usize");
        let slot = (self.current[level] + ratio) % SLOTS_PER_LEVEL;
        self.wheel[level][slot].push_back(id);
    }

    fn advance(&mut self) {
        self.base = self
            .base
            .checked_add(self.tick[0])
            .expect("timer base overflow");
        self.current[0] = (self.current[0] + 1) % SLOTS_PER_LEVEL;
        if self.current[0] == 0 {
            self.current[1] = (self.current[1] + 1) % SLOTS_PER_LEVEL;
            // Every entry in the newly-reached level-1 slot is overdue; place
            // them back into level 0 where the current poll collects them.
            let overdue = std::mem::take(&mut self.wheel[1][self.current[1]]);
            for id in overdue {
                if self.timers.contains_key(&id) {
                    self.place(id, Duration::ZERO);
                }
            }
        }
    }

    fn collect_current(&mut self, now: Instant, due: &mut Vec<TimerEvent>) {
        let slot = std::mem::take(&mut self.wheel[0][self.current[0]]);
        for id in slot {
            let Some(entry) = self.timers.remove(&id) else {
                continue; // cancelled while waiting in the slot
            };
            if entry.deadline <= now {
                due.push(TimerEvent {
                    id: entry.id,
                    token: entry.token,
                    kind: entry.kind,
                });
            } else {
                self.timers.insert(id, entry);
                self.wheel[0][self.current[0]].push_back(id);
            }
        }
    }

    fn sweep_overflow(&mut self, now: Instant, due: &mut Vec<TimerEvent>) {
        let limit = self.base.checked_add(self.span[2]).expect("span overflow");
        let mut parked = Vec::new();
        for entry in std::mem::take(&mut self.overflow) {
            if entry.deadline <= now {
                self.timers.remove(&entry.id);
                due.push(TimerEvent {
                    id: entry.id,
                    token: entry.token,
                    kind: entry.kind,
                });
            } else if entry.deadline < limit {
                self.place(
                    entry.id,
                    entry.deadline.saturating_duration_since(self.base),
                );
            } else {
                parked.push(entry);
            }
        }
        self.overflow = parked;
    }
}

impl Default for TimerWheel {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{TimeoutKind, TimerWheel};
    use crate::platform::Token;
    use std::time::{Duration, Instant};

    fn token(n: u32) -> Token {
        Token::from_parts(n, 1)
    }

    #[test]
    fn insert_is_visible() {
        let mut wheel = TimerWheel::new();
        assert!(wheel.is_empty());
        wheel.insert(
            token(1),
            Instant::now() + Duration::from_secs(1),
            TimeoutKind::Idle,
        );
        assert_eq!(wheel.len(), 1);
        assert!(!wheel.is_empty());
        assert!(wheel.next_deadline().is_some());
    }

    #[test]
    fn due_timer_fires() {
        let mut wheel = TimerWheel::new();
        let id = wheel.insert(
            token(1),
            Instant::now() + Duration::from_millis(50),
            TimeoutKind::Idle,
        );
        let events = wheel.poll(Instant::now() + Duration::from_millis(100));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, id);
        assert_eq!(events[0].token, token(1));
        assert_eq!(events[0].kind, TimeoutKind::Idle);
        assert!(wheel.is_empty());
    }

    #[test]
    fn pending_timer_does_not_fire() {
        let mut wheel = TimerWheel::new();
        wheel.insert(
            token(1),
            Instant::now() + Duration::from_secs(5),
            TimeoutKind::Idle,
        );
        let events = wheel.poll(Instant::now() + Duration::from_millis(100));
        assert!(events.is_empty());
        assert_eq!(wheel.len(), 1);
    }

    #[test]
    fn cancel_stops_expiry() {
        let mut wheel = TimerWheel::new();
        let id = wheel.insert(
            token(1),
            Instant::now() + Duration::from_millis(50),
            TimeoutKind::Idle,
        );
        assert!(wheel.cancel(id));
        assert!(!wheel.cancel(id), "second cancel reports missing");
        let events = wheel.poll(Instant::now() + Duration::from_millis(200));
        assert!(events.is_empty());
        assert!(wheel.is_empty());
    }

    #[test]
    fn level1_timer_fires() {
        let mut wheel = TimerWheel::new();
        // span[1] is 2.56 s at the default resolution: this lands on level 1.
        wheel.insert(
            token(2),
            Instant::now() + Duration::from_secs(3),
            TimeoutKind::BodyRead,
        );
        let events = wheel.poll(Instant::now() + Duration::from_secs(4));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].token, token(2));
        assert_eq!(events[0].kind, TimeoutKind::BodyRead);
    }

    #[test]
    fn overflow_timer_fires() {
        let mut wheel = TimerWheel::new();
        // span[2] is ~655 s: this must park in the overflow list.
        let long = Instant::now() + Duration::from_secs(700);
        wheel.insert(token(3), long, TimeoutKind::HealthCheck);
        let events = wheel.poll(Instant::now() + Duration::from_mins(12));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].token, token(3));
    }

    #[test]
    fn multiple_due_timers_all_fire() {
        let mut wheel = TimerWheel::new();
        wheel.insert(
            token(1),
            Instant::now() + Duration::from_secs(1),
            TimeoutKind::HeadRead,
        );
        wheel.insert(
            token(2),
            Instant::now() + Duration::from_secs(2),
            TimeoutKind::BodyRead,
        );
        wheel.insert(
            token(3),
            Instant::now() + Duration::from_secs(3),
            TimeoutKind::Idle,
        );
        let events = wheel.poll(Instant::now() + Duration::from_secs(4));
        let mut tokens: Vec<u32> = events.iter().map(|e| e.token.index()).collect();
        tokens.sort_unstable();
        assert_eq!(tokens, vec![1, 2, 3]);
        assert!(wheel.is_empty());
    }

    #[test]
    fn expiry_bounded_by_tick_resolution() {
        let mut wheel = TimerWheel::new();
        // Deadline 9 ms out: not due at +5 ms, due shortly after.
        wheel.insert(
            token(1),
            Instant::now() + Duration::from_millis(9),
            TimeoutKind::Accept,
        );
        assert!(
            wheel
                .poll(Instant::now() + Duration::from_millis(5))
                .is_empty()
        );
        assert!(
            !wheel
                .poll(Instant::now() + Duration::from_millis(20))
                .is_empty()
        );
    }

    #[test]
    fn kind_names_are_stable() {
        assert_eq!(TimeoutKind::ProxyConnect.as_str(), "proxy_connect");
        assert_eq!(TimeoutKind::HealthCheck.as_str(), "health_check");
    }
}
