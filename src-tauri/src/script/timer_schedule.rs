//! Timer state transitions and a cooperatively bounded execution loop.

use std::future::Future;
use std::sync::Mutex;
use std::time::Duration;

use tokio::time::Instant;

use super::order::OrderedTimer;

pub(super) const CALLBACK_BATCH: usize = 64;

tokio::task_local! {
    // A pointer is used only as an opaque identity while `&TimerControl` is
    // borrowed by the loop. It is never dereferenced or exposed to mSL.
    static CALLBACK_TIMER: usize;
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(super) enum PauseMode {
    #[default]
    Running,
    Execution,
    Countdown,
}

pub(super) struct TimerSnapshot {
    pub remaining: Option<u32>,
    pub pause: PauseMode,
    pub delay: Duration,
}

struct TimerState {
    remaining: Option<u32>,
    pause: PauseMode,
    deadline: Instant,
    frozen: Option<Duration>,
    execute_requests: u32,
    manual_at: Option<Instant>,
    revision: u64,
    cancelled: bool,
}

pub(super) struct TimerControl {
    state: Mutex<TimerState>,
    notify: tokio::sync::Notify,
    interval: Duration,
    catch_up: bool,
    pub ordered: Option<OrderedTimer>,
}

enum Plan {
    Stop,
    Ready,
    Wait(Option<Instant>),
}

struct Tick {
    execute: bool,
    last: bool,
    scheduled: bool,
    revision: u64,
}

impl TimerControl {
    pub fn new(
        reps: u32,
        deadline: Instant,
        interval: Duration,
        catch_up: bool,
        ordered: Option<OrderedTimer>,
    ) -> Self {
        Self {
            state: Mutex::new(TimerState {
                remaining: (reps != 0).then_some(reps),
                pause: PauseMode::Running,
                deadline,
                frozen: None,
                execute_requests: 0,
                manual_at: None,
                revision: 0,
                cancelled: false,
            }),
            notify: tokio::sync::Notify::new(),
            interval,
            catch_up,
            ordered,
        }
    }

    fn effective_deadline(state: &TimerState) -> Option<Instant> {
        if state.cancelled || state.remaining == Some(0) {
            return None;
        }
        let scheduled = (state.pause != PauseMode::Countdown).then_some(state.deadline);
        match (scheduled, state.manual_at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (one, None) | (None, one) => one,
        }
    }

    // Ordered state is published while holding this control's state mutex.
    // The arbiter never locks a control, so updates cannot reverse their order.
    fn publish(&self, state: &TimerState) {
        if let Some(ordered) = &self.ordered {
            ordered.update(Self::effective_deadline(state));
        }
    }

    pub fn execute(&self, now: Instant) {
        let identity = self as *const Self as usize;
        if CALLBACK_TIMER
            .try_with(|current| *current == identity)
            .unwrap_or(false)
        {
            return;
        }
        let mut state = self.state.lock().unwrap();
        if !state.cancelled && state.remaining != Some(0) {
            state.execute_requests = state.execute_requests.saturating_add(1);
            state.manual_at.get_or_insert(now);
            self.publish(&state);
        }
        drop(state);
        self.notify.notify_one();
    }

    pub fn pause(&self, countdown: bool, now: Instant) {
        let mut state = self.state.lock().unwrap();
        if countdown {
            if state.frozen.is_none() {
                state.frozen = Some(state.deadline.saturating_duration_since(now));
            }
            state.pause = PauseMode::Countdown;
        } else {
            if let Some(remaining) = state.frozen.take() {
                match now.checked_add(remaining) {
                    Some(deadline) => state.deadline = deadline,
                    None => state.cancelled = true,
                }
            }
            state.pause = PauseMode::Execution;
        }
        state.revision = state.revision.wrapping_add(1);
        self.publish(&state);
        drop(state);
        self.notify.notify_one();
    }

    pub fn resume(&self, now: Instant) {
        let mut state = self.state.lock().unwrap();
        if let Some(remaining) = state.frozen.take() {
            match now.checked_add(remaining) {
                Some(deadline) => state.deadline = deadline,
                None => state.cancelled = true,
            }
        }
        state.pause = PauseMode::Running;
        state.revision = state.revision.wrapping_add(1);
        self.publish(&state);
        drop(state);
        self.notify.notify_one();
    }

    pub fn cancel(&self) {
        let mut state = self.state.lock().unwrap();
        state.cancelled = true;
        if let Some(ordered) = &self.ordered {
            ordered.cancel();
        }
        drop(state);
        self.notify.notify_one();
    }

    pub fn snapshot(&self, now: Instant) -> TimerSnapshot {
        let state = self.state.lock().unwrap();
        TimerSnapshot {
            remaining: state.remaining,
            pause: state.pause,
            delay: if state.cancelled {
                Duration::ZERO
            } else {
                state
                    .frozen
                    .unwrap_or_else(|| state.deadline.saturating_duration_since(now))
            },
        }
    }

    fn plan(&self, now: Instant) -> Plan {
        let state = self.state.lock().unwrap();
        if state.cancelled || state.remaining == Some(0) {
            Plan::Stop
        } else if state.execute_requests != 0 {
            Plan::Ready
        } else if state.pause == PauseMode::Countdown {
            Plan::Wait(None)
        } else if state.deadline <= now {
            Plan::Ready
        } else {
            Plan::Wait(Some(state.deadline))
        }
    }

    fn take_tick(&self, now: Instant) -> Option<Tick> {
        let mut state = self.state.lock().unwrap();
        if state.cancelled || state.remaining == Some(0) {
            return None;
        }
        let scheduled = state.execute_requests == 0;
        if scheduled {
            if state.pause == PauseMode::Countdown || state.deadline > now {
                return None;
            }
            let base = if self.catch_up { state.deadline } else { now };
            match base.checked_add(self.interval) {
                Some(next) => state.deadline = next,
                None => state.cancelled = true,
            }
        } else {
            state.execute_requests -= 1;
            if state.execute_requests == 0 {
                state.manual_at = None;
            }
        }
        // Finite repetitions count scheduled ticks even under execution-pause.
        // The callback sees the already-decremented remaining count.
        if let Some(remaining) = &mut state.remaining {
            *remaining = remaining.saturating_sub(1);
        }
        let tick = Tick {
            execute: !scheduled || state.pause == PauseMode::Running,
            last: state.remaining == Some(0) || state.cancelled,
            scheduled,
            revision: state.revision,
        };
        self.publish(&state);
        Some(tick)
    }

    fn complete_tick(&self, tick: &Tick, now: Instant) {
        let mut state = self.state.lock().unwrap();
        // Keep ordinary timers' existing delay-from-completion policy. A pause
        // or resume during the callback owns its clock and must not be replaced.
        if tick.scheduled && !self.catch_up && state.revision == tick.revision && !state.cancelled {
            match now.checked_add(self.interval) {
                Some(next) => state.deadline = next,
                None => state.cancelled = true,
            }
        }
        self.publish(&state);
    }
}

pub(super) async fn run_timer_loop<F, Fut>(control: &TimerControl, mut fire: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let mut ticks = 0usize;
    loop {
        let changed = control.notify.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        match control.plan(Instant::now()) {
            Plan::Stop => break,
            Plan::Wait(Some(deadline)) => {
                tokio::select! {
                    _ = tokio::time::sleep_until(deadline) => {},
                    _ = &mut changed => {},
                }
                continue;
            }
            Plan::Wait(None) => {
                changed.await;
                continue;
            }
            Plan::Ready => {}
        }
        let permit = if let Some(ordered) = &control.ordered {
            tokio::select! {
                biased;
                _ = &mut changed => continue,
                permit = ordered.acquire() => {
                    let Some(permit) = permit else { break };
                    Some(permit)
                },
            }
        } else {
            None
        };
        let Some(tick) = control.take_tick(Instant::now()) else {
            drop(permit);
            continue;
        };
        let keep_running = if tick.execute {
            let identity = control as *const TimerControl as usize;
            CALLBACK_TIMER.scope(identity, async { fire().await }).await
        } else {
            true
        };
        control.complete_tick(&tick, Instant::now());
        // Advance the recorded schedule before releasing the ordered turn.
        drop(permit);
        if tick.last || !keep_running {
            break;
        }
        ticks += 1;
        if control.interval.is_zero() || ticks == CALLBACK_BATCH {
            ticks = 0;
            tokio::task::yield_now().await;
        }
    }
    control.cancel();
}

#[cfg(test)]
#[path = "timer_schedule_tests.rs"]
mod tests;
