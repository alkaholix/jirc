use std::future::{ready, Future};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::Poll;

use super::*;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
}

#[test]
fn execution_pause_consumes_finite_due_ticks_without_callbacks() {
    runtime().block_on(async {
        let now = Instant::now();
        let control = TimerControl::new(
            3,
            now - Duration::from_secs(10),
            Duration::from_secs(1),
            true,
            None,
        );
        control.pause(false, now);
        let callbacks = AtomicUsize::new(0);
        run_timer_loop(&control, || {
            callbacks.fetch_add(1, Ordering::SeqCst);
            ready(true)
        })
        .await;
        assert_eq!(callbacks.load(Ordering::SeqCst), 0);
        assert_eq!(control.snapshot(Instant::now()).remaining, Some(0));
    });
}

#[test]
fn countdown_pause_freezes_metadata_and_resumes_exact_remaining_delay() {
    let now = Instant::now();
    let control = TimerControl::new(
        3,
        now + Duration::from_secs(10),
        Duration::from_secs(5),
        false,
        None,
    );
    control.pause(true, now + Duration::from_secs(3));
    for seconds in [4, 20, 100] {
        let snapshot = control.snapshot(now + Duration::from_secs(seconds));
        assert_eq!(snapshot.remaining, Some(3));
        assert_eq!(snapshot.pause, PauseMode::Countdown);
        assert_eq!(snapshot.delay, Duration::from_secs(7));
    }
    // Repeating -P must not replace the original frozen countdown.
    control.pause(true, now + Duration::from_secs(80));
    control.resume(now + Duration::from_secs(100));
    let snapshot = control.snapshot(now + Duration::from_secs(102));
    assert_eq!(snapshot.remaining, Some(3));
    assert_eq!(snapshot.pause, PauseMode::Running);
    assert_eq!(snapshot.delay, Duration::from_secs(5));
    assert_eq!(
        control.snapshot(now + Duration::from_secs(107)).delay,
        Duration::ZERO,
    );
}

#[test]
fn manual_execution_predecrements_repetitions_and_preserves_pause_and_countdown() {
    runtime().block_on(async {
        for mode in [
            PauseMode::Running,
            PauseMode::Execution,
            PauseMode::Countdown,
        ] {
            let now = Instant::now();
            let control = TimerControl::new(
                3,
                now + Duration::from_secs(10),
                Duration::from_secs(5),
                false,
                None,
            );
            match mode {
                PauseMode::Running => {}
                PauseMode::Execution => control.pause(false, now + Duration::from_secs(3)),
                PauseMode::Countdown => control.pause(true, now + Duration::from_secs(3)),
            }
            control.execute(now + Duration::from_secs(4));
            let callbacks = AtomicUsize::new(0);
            run_timer_loop(&control, || {
                callbacks.fetch_add(1, Ordering::SeqCst);
                let snapshot = control.snapshot(now + Duration::from_secs(4));
                assert_eq!(snapshot.remaining, Some(2), "mode {mode:?}");
                assert_eq!(snapshot.pause, mode);
                assert_eq!(
                    snapshot.delay,
                    Duration::from_secs(if mode == PauseMode::Countdown { 7 } else { 6 }),
                    "manual execution must not reset the existing countdown",
                );
                ready(false)
            })
            .await;
            assert_eq!(callbacks.load(Ordering::SeqCst), 1);
        }
    });
}

#[test]
fn callback_cancellation_stops_queued_manual_executions() {
    runtime().block_on(async {
        let now = Instant::now();
        let control = TimerControl::new(
            0,
            now + Duration::from_secs(3600),
            Duration::from_secs(1),
            false,
            None,
        );
        for _ in 0..4096 {
            control.execute(now);
        }
        let callbacks = AtomicUsize::new(0);
        run_timer_loop(&control, || {
            callbacks.fetch_add(1, Ordering::SeqCst);
            control.cancel();
            ready(true)
        })
        .await;
        assert_eq!(callbacks.load(Ordering::SeqCst), 1);
    });
}

#[test]
fn catch_up_preserves_overdue_debt_across_callback_batch_yields() {
    runtime().block_on(async {
        let interval = Duration::from_millis(10);
        let deadline = Instant::now() - Duration::from_secs(3600);
        let total = CALLBACK_BATCH + 1;
        let control = TimerControl::new(total as u32, deadline, interval, true, None);
        let callbacks = AtomicUsize::new(0);
        let (first_tx, first_rx) = tokio::sync::oneshot::channel();
        let mut first_tx = Some(first_tx);
        let worker = run_timer_loop(&control, || {
            let count = callbacks.fetch_add(1, Ordering::SeqCst) + 1;
            // Catch-up advances by one interval per occurrence, even after a
            // fairness yield. It must not discard debt by jumping to now.
            assert_eq!(
                control.state.lock().unwrap().deadline,
                deadline + interval * count as u32,
            );
            if let Some(first_tx) = first_tx.take() {
                let _ = first_tx.send(());
            }
            ready(true)
        });
        let peer = async {
            first_rx.await.unwrap();
            callbacks.load(Ordering::SeqCst)
        };
        let (_, first_progress) = tokio::join!(biased; worker, peer);
        assert!((1..=CALLBACK_BATCH).contains(&first_progress));
        assert_eq!(callbacks.load(Ordering::SeqCst), total);
        assert_eq!(control.snapshot(Instant::now()).remaining, Some(0));
    });
}

#[test]
fn past_initial_deadline_is_due_without_restarting_the_interval() {
    runtime().block_on(async {
        let control = TimerControl::new(
            1,
            Instant::now() - Duration::from_secs(1),
            Duration::from_secs(3600),
            false,
            None,
        );
        let callbacks = AtomicUsize::new(0);
        let mut task = Box::pin(run_timer_loop(&control, || {
            callbacks.fetch_add(1, Ordering::SeqCst);
            ready(true)
        }));
        // A finite one-shot whose saved deadline is past completes on its
        // first poll; a newly computed interval would instead return Pending.
        std::future::poll_fn(|cx| {
            assert!(task.as_mut().poll(cx).is_ready());
            Poll::Ready(())
        })
        .await;
        assert_eq!(callbacks.load(Ordering::SeqCst), 1);
    });
}

#[test]
fn callback_pause_and_resume_own_the_clock_after_callback_completion() {
    runtime().block_on(async {
        for resume in [false, true] {
            let now = Instant::now();
            let control = TimerControl::new(
                0,
                now - Duration::from_secs(1),
                Duration::from_secs(20),
                false,
                None,
            );
            let expected_deadline = Mutex::new(None);
            run_timer_loop(&control, || {
                let current = control.state.lock().unwrap().deadline;
                control.pause(true, current - Duration::from_secs(7));
                assert_eq!(control.snapshot(now).delay, Duration::from_secs(7));
                let expected = if resume {
                    let resumed_at = now + Duration::from_secs(100);
                    control.resume(resumed_at);
                    resumed_at + Duration::from_secs(7)
                } else {
                    current
                };
                *expected_deadline.lock().unwrap() = Some(expected);
                ready(false)
            })
            .await;
            assert_eq!(
                control.state.lock().unwrap().deadline,
                expected_deadline.lock().unwrap().unwrap(),
                "pause/resume during the callback must survive completion",
            );
            assert_eq!(
                control.state.lock().unwrap().pause,
                if resume {
                    PauseMode::Running
                } else {
                    PauseMode::Countdown
                },
            );
        }
    });
}

#[test]
fn manual_execute_requested_inside_its_callback_does_not_reenter() {
    runtime().block_on(async {
        let now = Instant::now();
        let control = TimerControl::new(
            0,
            now + Duration::from_secs(3600),
            Duration::from_secs(1),
            false,
            None,
        );
        control.execute(now);
        let callbacks = AtomicUsize::new(0);
        let (first_tx, first_rx) = tokio::sync::oneshot::channel();
        let mut first_tx = Some(first_tx);
        let worker = run_timer_loop(&control, || {
            callbacks.fetch_add(1, Ordering::SeqCst);
            control.execute(Instant::now());
            if let Some(first_tx) = first_tx.take() {
                let _ = first_tx.send(());
            }
            ready(true)
        });
        let peer = async {
            first_rx.await.unwrap();
            let first_progress = callbacks.load(Ordering::SeqCst);
            control.cancel();
            first_progress
        };
        let (_, first_progress) = tokio::join!(biased; worker, peer);
        assert_eq!(first_progress, 1);
        assert_eq!(callbacks.load(Ordering::SeqCst), 1);
    });
}

#[test]
fn external_manual_request_during_a_held_callback_runs_after_it_finishes() {
    runtime().block_on(async {
        let now = Instant::now();
        let control = TimerControl::new(
            2,
            now + Duration::from_secs(3600),
            Duration::from_secs(1),
            false,
            None,
        );
        control.execute(now);
        let callbacks = AtomicUsize::new(0);
        let (first_tx, first_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let mut first_tx = Some(first_tx);
        let mut release_rx = Some(release_rx);
        let worker = run_timer_loop(&control, || {
            let count = callbacks.fetch_add(1, Ordering::SeqCst) + 1;
            assert!(count <= 2);
            if let Some(first_tx) = first_tx.take() {
                first_tx.send(()).unwrap();
            }
            let release_rx = release_rx.take();
            async move {
                if let Some(release_rx) = release_rx {
                    release_rx.await.unwrap();
                }
                true
            }
        });
        let peer = async {
            first_rx.await.unwrap();
            assert_eq!(callbacks.load(Ordering::SeqCst), 1);
            // This peer is polled outside the callback's task-local scope.
            // Its request must queue while that callback is still suspended.
            control.execute(Instant::now());
            release_tx.send(()).unwrap();
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(biased; worker, peer);
        })
        .await
        .expect("an external request must not be lost while a callback is suspended");
        assert_eq!(callbacks.load(Ordering::SeqCst), 2);
        assert_eq!(control.snapshot(Instant::now()).remaining, Some(0));
    });
}

#[test]
fn timer_activation_drop_releases_due_work_and_respects_prior_cancellation() {
    runtime().block_on(async {
        for cancelled_before_activation in [false, true] {
            let control = TimerControl::new(
                1,
                Instant::now() - Duration::from_secs(1),
                Duration::from_secs(1),
                false,
                None,
            );
            let gate = std::sync::Arc::new(tokio::sync::Notify::new());
            let activation = super::super::TimerActivation { gate: gate.clone() };
            let callbacks = AtomicUsize::new(0);
            let mut task = Box::pin(async {
                gate.notified().await;
                run_timer_loop(&control, || {
                    callbacks.fetch_add(1, Ordering::SeqCst);
                    ready(true)
                })
                .await;
            });
            // The saved deadline is already due, but the outer action list
            // still owns activation, so polling the task cannot fire it.
            std::future::poll_fn(|cx| {
                assert!(task.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            assert_eq!(callbacks.load(Ordering::SeqCst), 0);
            if cancelled_before_activation {
                control.cancel();
            }
            drop(activation);
            task.await;
            assert_eq!(
                callbacks.load(Ordering::SeqCst),
                usize::from(!cancelled_before_activation),
            );
        }
    });
}

fn measure_manual_burst(legacy_loop_model: bool) -> serde_json::Value {
    use crate::script::eval::Action;
    use crate::script::{RunCtx, ScriptEngine};

    const REQUESTS: usize = 4096;
    runtime().block_on(async {
        let engine = ScriptEngine::new();
        let context = RunCtx {
            server_id: "",
            my_nick: "BenchNick",
            network: "BenchNet",
            server: "bench.invalid",
            data_dir: std::env::temp_dir(),
            state: std::sync::Arc::new(Default::default()),
        };
        assert!(engine.run_command(&context, "#bench", "set %ticks 0", &[]).is_empty());
        let now = Instant::now();
        let control = TimerControl::new(
            REQUESTS as u32,
            now + Duration::from_secs(3600),
            Duration::from_secs(1),
            false,
            None,
        );
        for _ in 0..REQUESTS {
            control.execute(now);
        }
        let callbacks = AtomicUsize::new(0);
        let (first_tx, first_rx) = tokio::sync::oneshot::channel();
        let mut first_tx = Some(first_tx);
        let started = std::time::Instant::now();
        let mut fire = || {
            assert!(engine.run_timer_command(
                &context, "#bench", "inc %ticks", "", "benchmark",
            ).is_empty());
            callbacks.fetch_add(1, Ordering::SeqCst);
            if let Some(first_tx) = first_tx.take() {
                let _ = first_tx.send(());
            }
            ready(true)
        };
        let worker = async {
            if legacy_loop_model {
                // Model the previous timer.rs manual-request drain: await a
                // ready callback, then consume a repetition, with no yield.
                // This is not the complete legacy Tauri/app timer backend.
                let mut remaining = REQUESTS;
                for _ in 0..REQUESTS {
                    if !fire().await {
                        break;
                    }
                    remaining -= 1;
                    if remaining == 0 {
                        break;
                    }
                }
            } else {
                run_timer_loop(&control, &mut fire).await;
            }
        };
        let peer = async {
            first_rx.await.unwrap();
            (callbacks.load(Ordering::SeqCst), started.elapsed())
        };
        // Poll the timer first, then the sibling future at its first yield.
        // This gives a deterministic scheduling comparison without asserting
        // a machine-dependent latency threshold.
        let (_, (first_progress, first_elapsed)) = tokio::join!(biased; worker, peer);
        let completed_elapsed = started.elapsed();
        assert_eq!(callbacks.load(Ordering::SeqCst), REQUESTS);
        assert_eq!(
            engine.run_command(&context, "#bench", "echo -a %ticks", &[]),
            vec![Action::Echo { target: "(active)".into(), text: REQUESTS.to_string() }],
        );
        if legacy_loop_model {
            assert_eq!(first_progress, REQUESTS);
        } else {
            assert!((1..=CALLBACK_BATCH).contains(&first_progress));
        }
        serde_json::json!({
            "implementation": if legacy_loop_model { "legacy-loop-model" } else { "production-run_timer_loop" },
            "callback": "actual ScriptEngine::run_timer_command: inc %ticks",
            "requests": REQUESTS,
            "completed_callbacks": callbacks.load(Ordering::SeqCst),
            "peer_first_progress_callbacks": first_progress,
            "peer_first_progress_ms": first_elapsed.as_secs_f64() * 1000.0,
            "completion_ms": completed_elapsed.as_secs_f64() * 1000.0,
        })
    })
}

#[test]
fn manual_burst_yields_to_peer_with_actual_msl_callbacks_benchmark() {
    let result = serde_json::json!({
        "benchmark": "timer manual-burst cooperative responsiveness",
        "scope": "current-thread Tokio runtime with biased join sibling future; legacy-loop-model versus production loop; not a full-app benchmark",
        "callback_batch": CALLBACK_BATCH,
        "latency_threshold_asserted": false,
        "baseline": measure_manual_burst(true),
        "candidate": measure_manual_burst(false),
    });
    println!(
        "TIMER_RESPONSIVENESS_BENCH {}",
        serde_json::to_string(&result).unwrap()
    );
    if let Some(path) = std::env::var_os("JIRC_TIMER_BENCH_OUT") {
        std::fs::write(path, serde_json::to_vec_pretty(&result).unwrap()).unwrap();
    }
}
