//! Shared event selection and synchronous handler execution. A nested event
//! reuses the current runtime's stores without relocking the script engine.

use std::collections::HashMap;
use std::sync::atomic::Ordering;

use super::ast::Stmt;
use super::eval::{EventVars, Runtime, CLIENT_DLEVEL_KEY, REMOTE_FLAGS_KEY};

pub(super) fn remote_enabled(vars: &HashMap<String, String>, kind: &str) -> bool {
    let remote_flags = vars
        .get(REMOTE_FLAGS_KEY)
        .and_then(|value| value.parse::<u8>().ok())
        .unwrap_or(7);
    let required_flag = if kind.eq_ignore_ascii_case("RAW") {
        4
    } else if kind.eq_ignore_ascii_case("CTCP") || kind.eq_ignore_ascii_case("CTCPREPLY") {
        1
    } else {
        2
    };
    remote_flags & required_flag != 0
}

pub(super) fn dispatch_handlers(
    rt: &mut Runtime<'_>,
    kind: &str,
    event: &EventVars,
    phase: Option<bool>,
) -> (bool, bool) {
    if !remote_enabled(rt.vars, kind) {
        return (false, false);
    }

    let mut event = event.clone();
    event.event = kind.to_ascii_lowercase();
    if event.event_id.is_empty() {
        event.event_id = super::NEXT_EVENT_ID
            .fetch_add(1, Ordering::Relaxed)
            .to_string();
    }
    let mut event_sources = Vec::new();
    for handler in rt.script.events_of(kind) {
        if !event.event_source_filter.is_empty()
            && !handler
                .source
                .eq_ignore_ascii_case(&event.event_source_filter)
        {
            continue;
        }
        if !event_sources
            .iter()
            .any(|source: &String| source.eq_ignore_ascii_case(&handler.source))
        {
            event_sources.push(handler.source.clone());
        }
    }

    let mut halted = false;
    let mut default_halted = false;
    for early_pass in [true, false] {
        if phase.is_some_and(|wanted| wanted != early_pass) {
            continue;
        }
        for source in &event_sources {
            let mut candidates = Vec::new();
            let mut highest_numeric: Option<i64> = None;
            for handler in rt
                .script
                .events_of(kind)
                .filter(|handler| handler.source.eq_ignore_ascii_case(source))
            {
                if !rt.script.group_enabled(rt.vars, &handler.group) {
                    continue;
                }
                let access = super::event_access(&handler.level);
                if access.early != early_pass || (access.skip_if_halted && halted) {
                    continue;
                }
                let pattern = super::expand_event_vars(&handler.pattern, rt.vars);
                let selector = super::expand_event_vars(&handler.selector, rt.vars);
                let target = super::expand_event_vars(&handler.target, rt.vars);
                if !super::matches(
                    &event,
                    &pattern,
                    &selector,
                    &target,
                    kind,
                    access.regex_match,
                    &rt.state.isupport,
                ) {
                    continue;
                }
                let is_self = !event.nick.is_empty()
                    && rt.state.isupport.names_equal(&event.nick, &rt.my_nick);
                if (access.self_only && !is_self) || (access.exclude_self && is_self) {
                    continue;
                }
                if access.require_own_op {
                    let op_prefix = rt.state.isupport.prefix_for_mode('o').unwrap_or('@');
                    let own_is_op =
                        rt.state
                            .channels
                            .iter()
                            .find(|channel| {
                                let name = rt
                                    .state
                                    .isupport
                                    .channel_target(&event.chan)
                                    .unwrap_or(&event.chan);
                                rt.state.isupport.names_equal(&channel.name, name)
                            })
                            .and_then(|channel| {
                                channel.members.iter().find(|(nick, _)| {
                                    rt.state.isupport.names_equal(nick, &rt.my_nick)
                                })
                            })
                            .is_some_and(|(_, prefixes)| prefixes.contains(op_prefix));
                    if !own_is_op {
                        continue;
                    }
                }

                let mut handler_event = event.clone();
                let address = rt
                    .state
                    .ial
                    .iter()
                    .find(|(nick, _)| rt.state.isupport.names_equal(nick, &event.nick))
                    .map(|(_, address)| address.as_str())
                    .unwrap_or("");
                handler_event.match_key = pattern.clone();
                let status = rt
                    .state
                    .channels
                    .iter()
                    .find(|channel| {
                        let name = rt
                            .state
                            .isupport
                            .channel_target(&event.chan)
                            .unwrap_or(&event.chan);
                        rt.state.isupport.names_equal(&channel.name, name)
                    })
                    .and_then(|channel| {
                        channel
                            .members
                            .iter()
                            .find(|(nick, _)| rt.state.isupport.names_equal(nick, &event.nick))
                    })
                    .map(|(_, prefixes)| prefixes.as_str())
                    .unwrap_or("");
                let default_level = rt.vars.get(CLIENT_DLEVEL_KEY)
                    .and_then(|value| value.parse::<u16>().ok()).unwrap_or(1);
                let levels = rt.users.levels_of_with_default(&event.nick, address, default_level);
                match super::users::level_matches(&access.level, &levels, status) {
                    Some((clevel, ulevel)) => {
                        handler_event.clevel = clevel;
                        handler_event.matched_address = rt
                            .users
                            .matched_address_for(&event.nick, address, &ulevel)
                            .unwrap_or(address)
                            .to_string();
                        handler_event.ulevel = ulevel;
                    }
                    None => continue,
                }
                handler_event.script_source = handler.source.clone();
                handler_event.script_line = handler.source_line;
                handler_event.default_halted = default_halted;
                handler_event.event_halted = halted;
                let rank = super::event_level_rank(&access.level);
                if let Some(rank) = rank {
                    highest_numeric = Some(highest_numeric.map_or(rank, |old| old.max(rank)));
                }
                candidates.push((handler.body.clone(), access, handler_event, rank));
            }

            // Collect per-file candidates before running any of them, matching
            // the ordinary dispatcher even if a handler changes group state.
            for (body, access, mut handler_event, rank) in candidates {
                if rank.is_some() && rank != highest_numeric {
                    continue;
                }
                if access.skip_if_halted && halted {
                    continue;
                }
                handler_event.event_halted = halted;
                handler_event.default_halted = default_halted;
                let (handler_halted, handler_default_halted) =
                    rt.run_event_handler(&body, handler_event);
                let handler_default_halted =
                    handler_default_halted || (access.early && handler_halted);
                default_halted |= handler_default_halted;
                halted |= handler_halted || handler_default_halted;
            }
        }
    }
    (halted, default_halted)
}

impl Runtime<'_> {
    fn run_event_handler(&mut self, body: &[Stmt], event: EventVars) -> (bool, bool) {
        let connection = self.enter_connection_routine();
        let saved_event = std::mem::replace(&mut self.event, event);
        let saved_locals = std::mem::replace(&mut self.local_scopes, vec![HashMap::new()]);
        let saved_caller = std::mem::replace(&mut self.caller, "event");
        let saved_show = std::mem::replace(&mut self.show, true);
        let saved_halted = std::mem::replace(&mut self.halted, false);
        let saved_return = self.ret.take();
        let saved_goto = self.goto.take();
        let saved_pipes = std::mem::take(&mut self.pending_pipe_commands);
        let saved_alias_stack = std::mem::take(&mut self.alias_stack);
        let saved_error = self.script_error.take();
        // Each handler is its own scope for `$*`.
        let saved_star_token = self.star_token.take();
        let saved_star_done = std::mem::replace(&mut self.star_done, 0);
        let saved_star_tape = self.star_tape.take();

        // Keep steps/depth and shared stores on this runtime. A signal handler
        // can change globals before its caller evaluates the next expression.
        self.run(body);
        // Nested SIGNAL handlers share a runtime but have independent error
        // handlers and must not abort or overwrite their calling routine.
        self.finish_script_error();
        let halted = self.halted && self.ret.is_none();
        let default_halted = self.event.default_halted;

        self.event = saved_event;
        self.local_scopes = saved_locals;
        self.caller = saved_caller;
        self.show = saved_show;
        self.halted = saved_halted;
        self.ret = saved_return;
        self.goto = saved_goto;
        self.pending_pipe_commands = saved_pipes;
        self.alias_stack = saved_alias_stack;
        self.script_error = saved_error;
        self.star_token = saved_star_token;
        self.star_done = saved_star_done;
        self.star_tape = saved_star_tape;
        self.leave_connection_routine(connection);
        (halted, default_halted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::eval::Action;
    use crate::script::{RunCtx, ScriptEngine};

    fn context() -> RunCtx<'static> {
        RunCtx {
            server_id: "",
            my_nick: "me",
            network: "TestNet",
            server: "irc.test.invalid",
            data_dir: std::env::temp_dir(),
            state: std::sync::Arc::new(crate::irc::state::StateSnapshot {
                server_id: "test".into(),
                nick: "me".into(),
                ..Default::default()
            }),
        }
    }

    fn echo_text(actions: Vec<Action>) -> Vec<String> {
        actions
            .into_iter()
            .map(|action| match action {
                Action::Echo { target, text } if target == "(active)" => text,
                other => panic!("expected an active-window echo, got {other:?}"),
            })
            .collect()
    }

    #[test]
    fn immediate_signal_changes_later_expressions_and_output_order() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias entry { set %state before | echo -a before | signal -n change payload | echo -a after= $+ %state }\n\
             on *:SIGNAL:change:{ echo -a inside= $+ $1 | set %state changed }",
        );
        assert_eq!(
            echo_text(engine.run_alias(&context(), "#room", "entry", "")),
            vec!["before", "inside=payload", "after=changed"]
        );
    }

    #[test]
    fn immediate_signal_isolates_locals_and_restores_caller_event_and_source() {
        let engine = ScriptEngine::new();
        engine.load_sources(&[
            (
                "caller.mrc".into(),
                "alias -l where { return caller }\n\
                 on *:TEXT:go*:#:{\n\
                   var %private = caller-local\n\
                   signal -n inspect child payload\n\
                   echo -a caller= $+ $1- local= $+ %private who= $+ $where child= $+ %childsource source= $+ $script event= $+ $event nick= $+ $nick chan= $+ $chan target= $+ $target raw=[ $+ $rawmsg $+ ] bytes=[ $+ $rawbytes $+ ] numeric= $+ $numeric tags= $+ $msgtags(0) id= $+ $eventid signal=[ $+ $signal $+ ] caller= $+ $caller\n\
                 }"
                    .into(),
            ),
            (
                "handler.mrc".into(),
                "alias -l where { return handler }\n\
                 on *:SIGNAL:inspect:{\n\
                   echo -a child= $+ $1- local=[ $+ %private $+ ] who= $+ $where source= $+ $script event= $+ $event nick= $+ $nick chan= $+ $chan target= $+ $target raw=[ $+ $rawmsg $+ ] bytes=[ $+ $rawbytes $+ ] numeric=[ $+ $numeric $+ ] tags= $+ $msgtags(0) id= $+ $iif($eventid == caller-id,inherited,fresh) signal= $+ $signal caller= $+ $caller\n\
                   var %private = handler-local\n\
                   set %childsource $where\n\
                 }"
                    .into(),
            ),
        ]);
        let event = EventVars {
            nick: "alice".into(),
            chan: "#room".into(),
            target: "#room".into(),
            text: "go original".into(),
            params: vec!["go".into(), "original".into()],
            event_id: "caller-id".into(),
            event_source_filter: "caller.mrc".into(),
            raw_msg: "WIRE".into(),
            raw_bytes: b"WIRE".to_vec(),
            numeric: "353".into(),
            msg_tags: vec![("account".into(), "alice".into(), true)],
            msg_tags_raw: "account=alice".into(),
            ..Default::default()
        };
        assert_eq!(
            echo_text(engine.dispatch_event(&context(), "TEXT", event)),
            vec![
                "child=child payload local=[] who=handler source=handler.mrc event=signal nick=me chan=inspect target=inspect raw=[] bytes=[] numeric=[] tags=0 id=fresh signal=inspect caller=event",
                "caller=go original local=caller-local who=caller child=handler source=caller.mrc event=text nick=alice chan=#room target=#room raw=[WIRE] bytes=[WIRE] numeric=353 tags=1 id=caller-id signal=[] caller=event",
            ]
        );
    }

    #[test]
    fn immediate_signal_handler_exit_states_do_not_abort_the_caller() {
        for (control, continues, next_halted) in [
            ("halt", false, "$true"),
            ("return ignored", false, "$false"),
            ("haltdef", true, "$true"),
        ] {
            let engine = ScriptEngine::new();
            engine.load_sources(&[
                (
                    "caller.mrc".into(),
                    "alias entry { echo -a before | signal -n stop | echo -a after- $+ $halted }"
                        .into(),
                ),
                (
                    "first.mrc".into(),
                    format!("on *:SIGNAL:stop:{{ echo -a first | {control} | echo -a tail }}"),
                ),
                (
                    "second.mrc".into(),
                    "on *:SIGNAL:stop:echo -a second- $+ $halted".into(),
                ),
            ]);
            let mut expected = vec!["before".to_string(), "first".to_string()];
            if continues {
                expected.push("tail".into());
            }
            expected.push(format!("second-{next_halted}"));
            expected.push("after-$false".into());
            assert_eq!(
                echo_text(engine.run_alias(&context(), "#room", "entry", "")),
                expected,
                "handler command {control}",
            );
        }
    }

    #[test]
    fn immediate_signal_respects_remote_and_group_gates() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias entry { signal -n enabled.first }\n\
             #listeners on\n\
             on *:SIGNAL:enabled.*:inc %hits\n\
             #listeners end",
        );
        let hits = || {
            engine
                .inner
                .lock()
                .unwrap()
                .vars
                .get("hits")
                .cloned()
                .unwrap_or_default()
        };
        engine.run_command(&context(), "", "set %hits 0", &[]);
        engine.run_alias(&context(), "", "entry", "");
        assert_eq!(hits(), "1");
        engine.run_command(&context(), "", "events off", &[]);
        engine.run_alias(&context(), "", "entry", "");
        assert_eq!(hits(), "1");
        engine.run_command(&context(), "", "events on", &[]);
        engine.run_command(&context(), "", "disable #listeners", &[]);
        engine.run_alias(&context(), "", "entry", "");
        assert_eq!(hits(), "1");
        engine.run_command(&context(), "", "enable #listeners", &[]);
        engine.run_alias(&context(), "", "entry", "");
        assert_eq!(hits(), "2");
    }

    #[test]
    fn immediate_signal_limits_nested_signals_to_twenty_four() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias entry { set %hits 0 | signal -n recurse | echo -a first= $+ %hits | signal -n recurse | echo -a second= $+ %hits }\n\
             on *:SIGNAL:recurse:{ inc %hits | signal -n recurse }",
        );
        assert_eq!(
            echo_text(engine.run_alias(&context(), "#room", "entry", "")),
            vec!["first=24", "second=48"]
        );
    }

    #[test]
    fn immediate_signal_preserves_caller_binary_and_end_of_run_resources() {
        let engine = ScriptEngine::new();
        engine.load(
            "alias entry {\n\
               bset &caller 1 65 66\n\
               set -u0 %temporary alive\n\
               hadd -mu0 transient key alive\n\
               signal -n inspect\n\
               echo -a caller= $+ %temporary hash= $+ $hget(transient,key) bytes= $+ $bvar(&caller,1,2)\n\
             }\n\
             on *:SIGNAL:inspect:{ echo -a handler= $+ %temporary hash= $+ $hget(transient,key) bytes= $+ $bvar(&caller,1,2) }",
        );
        assert_eq!(
            echo_text(engine.run_alias(&context(), "#room", "entry", "")),
            vec![
                "handler=alive hash=alive bytes=65 66",
                "caller=alive hash=alive bytes=65 66",
            ]
        );
        let state = engine.inner.lock().unwrap();
        assert!(!state.vars.contains_key("temporary"));
        assert!(!state.hashes["transient"].contains_key("key"));
        assert!(state.bins.get("&caller").is_none());
    }
}
