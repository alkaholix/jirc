//! Ordering for `-d` timers, independent of executor wake-up order.
//!
//! Timers register their deadlines before their tasks wait. Each round freezes
//! the due timers in deadline/creation order, giving every member one turn
//! before an overdue catch-up timer can enter another round.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;
use tokio::time::Instant;

#[derive(Clone, Default)]
pub(super) struct OrderedTimers {
    shared: Arc<Shared>,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    sequence: u64,
    entries: HashMap<u64, Entry>,
    cohort: VecDeque<u64>,
    active: Option<u64>,
}

struct Entry {
    deadline: Option<Instant>,
    changed: Arc<Notify>,
    queued: bool,
}

impl State {
    fn remove_from_cohort(&mut self, sequence: u64) {
        let Some(entry) = self.entries.get_mut(&sequence) else {
            return;
        };
        if !entry.queued {
            return;
        }
        entry.queued = false;
        if self.cohort.front() == Some(&sequence) {
            self.cohort.pop_front();
        } else {
            // Non-head pause/cancel is uncommon; ordinary callback completion
            // removes the front in constant time and never scans other ids.
            self.cohort.retain(|candidate| *candidate != sequence);
        }
    }

    fn begin_round(&mut self, now: Instant) {
        if self.active.is_some() || !self.cohort.is_empty() {
            return;
        }
        let mut due = self
            .entries
            .iter()
            .filter_map(|(&sequence, entry)| {
                entry
                    .deadline
                    .filter(|deadline| *deadline <= now)
                    .map(|deadline| (deadline, sequence))
            })
            .collect::<Vec<_>>();
        due.sort_unstable();
        for (_, sequence) in due {
            self.entries.get_mut(&sequence).unwrap().queued = true;
            self.cohort.push_back(sequence);
        }
    }

    fn next_notify(&self, now: Instant) -> Option<Arc<Notify>> {
        if self.active.is_some() {
            return None;
        }
        if let Some(sequence) = self.cohort.front() {
            return self
                .entries
                .get(sequence)
                .map(|entry| entry.changed.clone());
        }
        // Do not freeze a new round until acquire: another registration may
        // arrive before the next task runs with an earlier eligible deadline.
        self.entries
            .iter()
            .filter_map(|(&sequence, entry)| {
                entry
                    .deadline
                    .filter(|deadline| *deadline <= now)
                    .map(|deadline| ((deadline, sequence), entry.changed.clone()))
            })
            .min_by_key(|(key, _)| *key)
            .map(|(_, changed)| changed)
    }
}

impl Shared {
    fn unregister(&self, sequence: u64) {
        let (cancelled, next) = {
            let mut state = self.state.lock().unwrap();
            state.remove_from_cohort(sequence);
            let cancelled = state.entries.remove(&sequence).map(|entry| entry.changed);
            // An outstanding permit still owns callback serialization even
            // when its timer has been cancelled or its last handle dropped.
            (cancelled, state.next_notify(Instant::now()))
        };
        if let Some(cancelled) = cancelled {
            cancelled.notify_waiters();
        }
        if let Some(next) = next {
            next.notify_one();
        }
    }
}

impl OrderedTimers {
    pub(super) fn register(&self, deadline: Instant) -> OrderedTimer {
        let changed = Arc::new(Notify::new());
        let sequence = {
            let mut state = self.shared.state.lock().unwrap();
            state.sequence = state
                .sequence
                .checked_add(1)
                .expect("ordered timer sequence exhausted");
            let sequence = state.sequence;
            state.entries.insert(
                sequence,
                Entry {
                    deadline: Some(deadline),
                    changed: changed.clone(),
                    queued: false,
                },
            );
            sequence
        };
        OrderedTimer {
            registration: Arc::new(Registration {
                shared: self.shared.clone(),
                sequence,
                changed,
            }),
        }
    }
}

/// Clones belong to the same timer generation. Dropping the last owner removes
/// that generation; explicit cancellation invalidates all of its clones.
#[derive(Clone)]
pub(super) struct OrderedTimer {
    registration: Arc<Registration>,
}

struct Registration {
    shared: Arc<Shared>,
    sequence: u64,
    changed: Arc<Notify>,
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.shared.unregister(self.sequence);
    }
}

impl OrderedTimer {
    pub(super) fn sequence(&self) -> u64 {
        self.registration.sequence
    }

    /// Updates the effective deadline, or excludes a paused timer with None.
    /// Updating a served timer never inserts it back into the current round.
    pub(super) fn update(&self, deadline: Option<Instant>) {
        let shared = &self.registration.shared;
        let next = {
            let mut state = shared.state.lock().unwrap();
            let Some(entry) = state.entries.get_mut(&self.sequence()) else {
                return;
            };
            if entry.deadline == deadline {
                return;
            };
            entry.deadline = deadline;
            let now = Instant::now();
            if deadline.is_none_or(|deadline| deadline > now) {
                state.remove_from_cohort(self.sequence());
            }
            state.next_notify(now)
        };
        // Notify only this generation if its deadline changed, plus the next
        // eligible generation. Unrelated waiters stay asleep.
        self.registration.changed.notify_waiters();
        if let Some(next) = next {
            next.notify_one();
        }
    }

    pub(super) fn cancel(&self) {
        self.registration.shared.unregister(self.sequence());
    }

    /// Waits for this generation's turn. None means it was cancelled; paused
    /// timers wait until resumed or cancelled. No mutex is held by the permit.
    pub(super) async fn acquire(&self) -> Option<OrderedPermit> {
        let shared = &self.registration.shared;
        loop {
            // Arm before inspecting state so completion, pause and cancellation
            // notifications cannot be lost between the check and the wait.
            let changed = self.registration.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let next_deadline = {
                let mut state = shared.state.lock().unwrap();
                if !state.entries.contains_key(&self.sequence()) {
                    return None;
                }
                let now = Instant::now();
                state.begin_round(now);
                if state.active.is_none() && state.cohort.front() == Some(&self.sequence()) {
                    state.active = Some(self.sequence());
                    return Some(OrderedPermit {
                        shared: shared.clone(),
                        sequence: self.sequence(),
                    });
                }
                // Each future sleeps on its own deadline even while another
                // cohort is running. Once due, it waits only for its own turn.
                state
                    .entries
                    .get(&self.sequence())
                    .and_then(|entry| entry.deadline)
                    .filter(|deadline| *deadline > now)
            };
            if let Some(deadline) = next_deadline {
                tokio::select! {
                    _ = &mut changed => {}
                    _ = tokio::time::sleep_until(deadline) => {}
                }
            } else {
                changed.await;
            }
        }
    }
}

pub(super) struct OrderedPermit {
    shared: Arc<Shared>,
    sequence: u64,
}

impl Drop for OrderedPermit {
    fn drop(&mut self) {
        let next = {
            let mut state = self.shared.state.lock().unwrap();
            if state.active == Some(self.sequence) {
                state.active = None;
                // Pause/cancel/reschedule may already have removed this id.
                // Never remove the next timer's turn when that happened.
                state.remove_from_cohort(self.sequence);
            }
            state.next_notify(Instant::now())
        };
        if let Some(next) = next {
            next.notify_one();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::Poll;
    use std::time::Duration;

    fn due() -> Instant {
        Instant::now() - Duration::from_secs(1)
    }

    #[tokio::test]
    async fn equal_deadlines_follow_creation_order_when_polled_backwards() {
        let timers = OrderedTimers::default();
        let deadline = due();
        let first = timers.register(deadline);
        let second = timers.register(deadline);
        assert!(first.sequence() < second.sequence());
        let later = second.acquire();
        tokio::pin!(later);
        assert!(matches!(futures_util::poll!(&mut later), Poll::Pending));
        let permit = first.acquire().await.unwrap();
        assert!(matches!(futures_util::poll!(&mut later), Poll::Pending));
        first.update(None);
        drop(permit);
        assert!(tokio::time::timeout(Duration::from_secs(1), later)
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn earlier_deadline_wins_even_when_created_and_polled_later() {
        let timers = OrderedTimers::default();
        let deadline = due();
        let later_deadline = timers.register(deadline);
        let earlier_deadline = timers.register(deadline - Duration::from_secs(1));
        let later = later_deadline.acquire();
        tokio::pin!(later);
        assert!(matches!(futures_util::poll!(&mut later), Poll::Pending));
        let permit = earlier_deadline.acquire().await.unwrap();
        earlier_deadline.update(None);
        drop(permit);
        assert!(tokio::time::timeout(Duration::from_secs(1), later)
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn pausing_or_cancelling_the_head_wakes_its_successor() {
        for cancel in [false, true] {
            let timers = OrderedTimers::default();
            let deadline = due();
            let head = timers.register(deadline);
            let next = timers.register(deadline);
            let waiting = next.acquire();
            tokio::pin!(waiting);
            assert!(matches!(futures_util::poll!(&mut waiting), Poll::Pending));
            if cancel {
                head.cancel();
            } else {
                head.update(None);
            }
            let permit = tokio::time::timeout(Duration::from_secs(1), waiting)
                .await
                .unwrap()
                .unwrap();
            next.update(None);
            drop(permit);
            if cancel {
                assert!(head.acquire().await.is_none());
                head.update(Some(deadline));
                assert!(head.acquire().await.is_none());
            } else {
                head.update(Some(deadline));
                assert!(head.acquire().await.is_some());
            }
        }
    }

    #[tokio::test]
    async fn replacement_does_not_inherit_cancelled_generations_turn() {
        let timers = OrderedTimers::default();
        let deadline = due();
        let old = timers.register(deadline);
        let next = timers.register(deadline);
        let old_permit = old.acquire().await.unwrap();
        old.cancel();
        let replacement = timers.register(deadline);
        assert!(old.sequence() < replacement.sequence());
        let waiting = next.acquire();
        tokio::pin!(waiting);
        assert!(matches!(futures_util::poll!(&mut waiting), Poll::Pending));
        drop(old_permit);
        let next_permit = tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .unwrap()
            .unwrap();
        next.update(None);
        drop(next_permit);
        assert!(replacement.acquire().await.is_some());
        assert!(old.acquire().await.is_none());
    }

    #[tokio::test]
    async fn catch_up_timer_waits_for_the_rest_of_its_frozen_cohort() {
        let timers = OrderedTimers::default();
        let deadline = due();
        let first = timers.register(deadline);
        let second = timers.register(deadline);
        let permit = first.acquire().await.unwrap();
        first.update(Some(deadline - Duration::from_secs(1)));
        drop(permit);
        let catch_up = first.acquire();
        tokio::pin!(catch_up);
        assert!(matches!(futures_util::poll!(&mut catch_up), Poll::Pending));
        let permit = second.acquire().await.unwrap();
        second.update(None);
        drop(permit);
        assert!(tokio::time::timeout(Duration::from_secs(1), catch_up)
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn newly_registered_timer_waits_for_the_current_cohort_to_finish() {
        let timers = OrderedTimers::default();
        let deadline = due();
        let first = timers.register(deadline);
        let second = timers.register(deadline);
        let permit = first.acquire().await.unwrap();
        let newcomer = timers.register(deadline - Duration::from_secs(1));
        first.update(None);
        drop(permit);
        let waiting = newcomer.acquire();
        tokio::pin!(waiting);
        assert!(matches!(futures_util::poll!(&mut waiting), Poll::Pending));
        let permit = second.acquire().await.unwrap();
        second.update(None);
        drop(permit);
        assert!(tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn registrations_drop_only_with_the_last_clone_and_permits_still_serialize() {
        let timers = OrderedTimers::default();
        let deadline = due();
        let first = timers.register(deadline);
        let owner = first.clone();
        let second = timers.register(deadline);
        drop(first);
        let permit = owner.acquire().await.unwrap();
        drop(owner);
        let waiting = second.acquire();
        tokio::pin!(waiting);
        assert!(matches!(futures_util::poll!(&mut waiting), Poll::Pending));
        drop(permit);
        assert!(tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn callbacks_never_overlap_under_concurrent_acquisition() {
        let timers = OrderedTimers::default();
        let deadline = due();
        let registrations = (0..8)
            .map(|_| timers.register(deadline))
            .collect::<Vec<_>>();
        let active = Arc::new(AtomicUsize::new(0));
        let observed = Arc::new(Mutex::new(Vec::new()));
        let mut tasks = Vec::new();
        for timer in registrations.into_iter().rev() {
            let active = active.clone();
            let observed = observed.clone();
            tasks.push(tokio::spawn(async move {
                let permit = timer.acquire().await.unwrap();
                assert_eq!(active.fetch_add(1, Ordering::SeqCst), 0);
                tokio::task::yield_now().await;
                observed.lock().unwrap().push(timer.sequence());
                assert_eq!(active.fetch_sub(1, Ordering::SeqCst), 1);
                timer.update(None);
                drop(permit);
            }));
        }
        tokio::time::timeout(Duration::from_secs(1), async {
            for task in tasks {
                task.await.unwrap();
            }
        })
        .await
        .unwrap();
        assert_eq!(*observed.lock().unwrap(), (1..=8).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn resumed_future_deadline_wakes_without_another_timers_notification() {
        let timers = OrderedTimers::default();
        let timer = timers.register(due());
        timer.update(None);
        let waiting = timer.acquire();
        tokio::pin!(waiting);
        assert!(matches!(futures_util::poll!(&mut waiting), Poll::Pending));
        let deadline = Instant::now() + Duration::from_millis(5);
        timer.update(Some(deadline));
        assert!(tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .unwrap()
            .is_some());
        assert!(Instant::now() >= deadline);
    }

    #[tokio::test]
    async fn dense_cohort_notification_work_is_linear() {
        use std::future::Future;
        use std::sync::atomic::AtomicBool;
        use std::task::{Context, Wake, Waker};

        struct ReadyQueue {
            ready: Mutex<VecDeque<usize>>,
            queued: Vec<AtomicBool>,
            wakes: AtomicUsize,
        }
        struct TimerWake {
            index: usize,
            queue: Arc<ReadyQueue>,
        }
        impl Wake for TimerWake {
            fn wake(self: Arc<Self>) {
                self.wake_by_ref();
            }

            fn wake_by_ref(self: &Arc<Self>) {
                self.queue.wakes.fetch_add(1, Ordering::SeqCst);
                if !self.queue.queued[self.index].swap(true, Ordering::SeqCst) {
                    self.queue.ready.lock().unwrap().push_back(self.index);
                }
            }
        }

        for size in [32, 64, 128] {
            let ordered = OrderedTimers::default();
            let deadline = due();
            let timers = (0..size)
                .map(|_| ordered.register(deadline))
                .collect::<Vec<_>>();
            let mut futures = timers
                .iter()
                .map(|timer| Some(Box::pin(timer.acquire())))
                .collect::<Vec<_>>();
            let queue = Arc::new(ReadyQueue {
                ready: Mutex::new((0..size).rev().collect()),
                queued: (0..size).map(|_| AtomicBool::new(true)).collect(),
                wakes: AtomicUsize::new(0),
            });
            let wakers = (0..size)
                .map(|index| {
                    Waker::from(Arc::new(TimerWake {
                        index,
                        queue: queue.clone(),
                    }))
                })
                .collect::<Vec<_>>();
            let mut polls = 0;
            let mut completed = 0;
            // The first poll is deliberately in reverse creation order. Later
            // polls occur only after a wake, coalesced like an executor task.
            loop {
                let next = queue.ready.lock().unwrap().pop_front();
                let Some(index) = next else { break };
                queue.queued[index].store(false, Ordering::SeqCst);
                let Some(future) = futures[index].as_mut() else {
                    continue;
                };
                polls += 1;
                assert!(polls <= 2 * size, "{size} timers required {polls} polls");
                match future
                    .as_mut()
                    .poll(&mut Context::from_waker(&wakers[index]))
                {
                    Poll::Ready(Some(permit)) => {
                        futures[index] = None;
                        timers[index].update(None);
                        drop(permit);
                        completed += 1;
                    }
                    Poll::Ready(None) => panic!("timer cancelled unexpectedly"),
                    Poll::Pending => {}
                }
            }
            assert_eq!(completed, size, "arbiter stalled without a wakeup");
            assert!(queue.wakes.load(Ordering::SeqCst) <= size);
        }
    }
}
