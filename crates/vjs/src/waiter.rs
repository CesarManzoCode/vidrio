//! The waiter lists of `Atomics.wait`, `Atomics.waitAsync` and
//! `Atomics.notify` (ECMA-262 §25.4.3).
//!
//! One critical section guards every waiter list of the process: a mutex
//! over the lists, keyed by the shared block and the byte index, and one
//! condition variable that every notification wakes. A synchronous waiter
//! sleeps on the condition variable until its token is marked as woken or
//! its timeout passes. An asynchronous waiter belongs to an agent: a
//! notification records its token under that agent's id, and the agent
//! settles the promise the next time it looks ([`settle`]), from its job
//! loop or while it waits for work.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use crate::agent::Agent;
use crate::buffer::SharedBlock;
use crate::gc::{Trace, Tracer};
use crate::interp::promise::Capability;
use crate::value::Value;

/// A waiter list: the block (by identity) and the byte index.
type Location = (usize, usize);

struct Entry {
    token: u64,
    agent: u64,
    is_async: bool,
    /// Keeps the block (and so its identity) alive while the waiter is
    /// listed.
    _block: Arc<SharedBlock>,
}

#[derive(Default)]
struct State {
    lists: HashMap<Location, VecDeque<Entry>>,
    /// Synchronous waiters that were notified.
    woken: HashSet<u64>,
    /// Asynchronous waiters that were notified, by agent.
    async_woken: HashMap<u64, Vec<u64>>,
}

impl State {
    /// Removes the waiter `token` from its list; whether it was listed.
    fn remove(&mut self, location: Location, token: u64) -> bool {
        let Some(list) = self.lists.get_mut(&location) else {
            return false;
        };
        let Some(pos) = list.iter().position(|e| e.token == token) else {
            return false;
        };
        list.remove(pos);
        if list.is_empty() {
            self.lists.remove(&location);
        }
        true
    }
}

#[derive(Default)]
struct Waiters {
    state: Mutex<State>,
    cond: Condvar,
}

fn waiters() -> &'static Waiters {
    static WAITERS: OnceLock<Waiters> = OnceLock::new();
    WAITERS.get_or_init(Waiters::default)
}

fn next_token() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// A new agent signifier.
pub fn new_agent_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// The instant `ms` milliseconds from now, or `None` for a timeout too
/// large to represent (an infinite wait).
fn deadline_after(ms: f64) -> Option<Instant> {
    let d = Duration::try_from_secs_f64(ms.max(0.0) / 1000.0).ok()?;
    Instant::now().checked_add(d)
}

/// EnterCriticalSection(WL): held while the waiter reads the value it
/// compares, so a notification cannot slip in between.
pub struct CriticalSection {
    guard: MutexGuard<'static, State>,
}

pub fn enter_critical_section() -> CriticalSection {
    CriticalSection {
        guard: waiters().state.lock().unwrap_or_else(|e| e.into_inner()),
    }
}

impl CriticalSection {
    /// AddWaiter plus SuspendThisAgent: blocks until notified (`true`) or
    /// until `timeout_ms` passes (`false`; infinite if not finite).
    pub fn suspend(
        mut self,
        block: &Arc<SharedBlock>,
        byte_index: usize,
        agent: u64,
        timeout_ms: f64,
    ) -> bool {
        let location = (block.id(), byte_index);
        let token = next_token();
        self.guard
            .lists
            .entry(location)
            .or_default()
            .push_back(Entry {
                token,
                agent,
                is_async: false,
                _block: block.clone(),
            });
        let deadline = if timeout_ms.is_finite() {
            deadline_after(timeout_ms)
        } else {
            None
        };
        let cond = &waiters().cond;
        let mut guard = self.guard;
        loop {
            if guard.woken.remove(&token) {
                return true;
            }
            match deadline {
                Some(d) => {
                    let now = Instant::now();
                    if now >= d {
                        guard.remove(location, token);
                        return false;
                    }
                    guard = cond
                        .wait_timeout(guard, d - now)
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                }
                None => guard = cond.wait(guard).unwrap_or_else(|e| e.into_inner()),
            }
        }
    }

    /// AddWaiter of an asynchronous waiter of `agent`; its token.
    fn add_async(&mut self, block: &Arc<SharedBlock>, byte_index: usize, agent: u64) -> u64 {
        let token = next_token();
        self.guard
            .lists
            .entry((block.id(), byte_index))
            .or_default()
            .push_back(Entry {
                token,
                agent,
                is_async: true,
                _block: block.clone(),
            });
        token
    }

    /// RemoveWaiters plus NotifyWaiter for the first `count` waiters
    /// (`count` may be infinite); the number notified.
    pub fn notify(&mut self, block: &Arc<SharedBlock>, byte_index: usize, count: f64) -> usize {
        let location = (block.id(), byte_index);
        let mut n = 0;
        let state = &mut *self.guard;
        if let Some(list) = state.lists.get_mut(&location) {
            while (n as f64) < count {
                let Some(e) = list.pop_front() else {
                    break;
                };
                if e.is_async {
                    state.async_woken.entry(e.agent).or_default().push(e.token);
                } else {
                    state.woken.insert(e.token);
                }
                n += 1;
            }
            if list.is_empty() {
                state.lists.remove(&location);
            }
        }
        if n > 0 {
            waiters().cond.notify_all();
        }
        n
    }
}

/// An asynchronous waiter of this agent whose promise is still pending.
struct Pending {
    token: u64,
    location: Location,
    deadline: Option<Instant>,
    capability: Capability,
}

/// The asynchronous waiters of one agent.
pub struct AsyncWaits {
    agent: u64,
    pending: Vec<Pending>,
}

impl AsyncWaits {
    pub fn new() -> AsyncWaits {
        AsyncWaits {
            agent: new_agent_id(),
            pending: Vec::new(),
        }
    }

    /// The agent signifier used in the waiter lists.
    pub fn agent_id(&self) -> u64 {
        self.agent
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

impl Default for AsyncWaits {
    fn default() -> AsyncWaits {
        AsyncWaits::new()
    }
}

impl Trace for AsyncWaits {
    fn trace(&self, tracer: &mut Tracer) {
        for p in &self.pending {
            p.capability.trace(tracer);
        }
    }
}

impl Drop for AsyncWaits {
    fn drop(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let mut state = waiters().state.lock().unwrap_or_else(|e| e.into_inner());
        for p in &self.pending {
            state.remove(p.location, p.token);
        }
        state.async_woken.remove(&self.agent);
    }
}

/// Lists an asynchronous waiter of `agent` (inside the critical section
/// `cs`) whose promise `capability` settles with "ok" when notified or
/// "timed-out" after `timeout_ms` (never if not finite).
pub fn add_async_waiter(
    agent: &mut Agent,
    cs: &mut CriticalSection,
    block: &Arc<SharedBlock>,
    byte_index: usize,
    timeout_ms: f64,
    capability: Capability,
) {
    let id = agent.async_waits.agent;
    let token = cs.add_async(block, byte_index, id);
    let deadline = if timeout_ms.is_finite() {
        deadline_after(timeout_ms)
    } else {
        None
    };
    agent.async_waits.pending.push(Pending {
        token,
        location: (block.id(), byte_index),
        deadline,
        capability,
    });
}

/// How long [`settle`] may block when nothing is ready.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AsyncWait {
    /// Never block.
    No,
    /// Block until the earliest timeout, but not on waiters without one.
    Timeouts,
    /// Block until a notification or a timeout, however long.
    Forever,
}

/// Settles the promises of the agent's asynchronous waiters that were
/// notified ("ok") or whose timeout passed ("timed-out"); whether any
/// settled. With `wait` it first blocks until one is ready.
pub fn settle(agent: &mut Agent, wait: AsyncWait) -> bool {
    if agent.async_waits.pending.is_empty() {
        return false;
    }
    let id = agent.async_waits.agent;
    let mut ready: Vec<(u64, &'static str)> = Vec::new();
    {
        let cond = &waiters().cond;
        let mut state = waiters().state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(tokens) = state.async_woken.remove(&id) {
                ready.extend(tokens.into_iter().map(|t| (t, "ok")));
            }
            let now = Instant::now();
            for p in &agent.async_waits.pending {
                if p.deadline.is_some_and(|d| d <= now) && state.remove(p.location, p.token) {
                    ready.push((p.token, "timed-out"));
                }
            }
            if !ready.is_empty() || wait == AsyncWait::No {
                break;
            }
            let earliest = agent
                .async_waits
                .pending
                .iter()
                .filter_map(|p| p.deadline)
                .min();
            match earliest {
                Some(d) => {
                    state = cond
                        .wait_timeout(state, d.saturating_duration_since(now))
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                }
                None if wait == AsyncWait::Forever => {
                    state = cond.wait(state).unwrap_or_else(|e| e.into_inner());
                }
                None => break,
            }
        }
    }
    for &(token, result) in &ready {
        let Some(pos) = agent
            .async_waits
            .pending
            .iter()
            .position(|p| p.token == token)
        else {
            continue;
        };
        let p = agent.async_waits.pending.remove(pos);
        let v = Value::String(agent.heap.alloc_str(result));
        // The resolving functions of an intrinsic promise do not throw.
        let _ = crate::ops::call(agent, p.capability.resolve, Value::Undefined, &[v]);
    }
    !ready.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notify_wakes_waiters_in_order_and_counts_them() {
        let block = SharedBlock::new(8, 8);
        let mut cs = enter_critical_section();
        let a = cs.add_async(&block, 0, 77);
        let b = cs.add_async(&block, 0, 77);
        let c = cs.add_async(&block, 4, 77);
        assert_eq!(cs.notify(&block, 0, 1.0), 1);
        assert_eq!(cs.guard.async_woken.get(&77), Some(&vec![a]));
        assert_eq!(cs.notify(&block, 0, f64::INFINITY), 1);
        assert_eq!(cs.guard.async_woken.get(&77), Some(&vec![a, b]));
        assert_eq!(cs.notify(&block, 0, f64::INFINITY), 0);
        assert!(cs.guard.remove((block.id(), 4), c));
        cs.guard.async_woken.remove(&77);
    }

    #[test]
    fn a_synchronous_wait_times_out() {
        let block = SharedBlock::new(8, 8);
        let cs = enter_critical_section();
        let start = Instant::now();
        assert!(!cs.suspend(&block, 0, 1, 20.0));
        assert!(start.elapsed() >= Duration::from_millis(20));
        let cs = enter_critical_section();
        assert!(!cs.guard.lists.contains_key(&(block.id(), 0)));
    }
}
