use crate::irc::state::{StateSnapshot, StateStore};

use super::super::{dispatch_parseline, Action, EventVars, RunCtx, ScriptEngine};

#[derive(Clone, Debug, PartialEq)]
struct Route {
    server_id: String,
    my_nick: String,
    network: String,
    server: String,
}

fn route(id: &str) -> Route {
    let (nick, network, server) = match id {
        "alpha" => ("AlphaNick", "AlphaNet", "irc.alpha.invalid"),
        "beta" => ("BetaNick", "BetaNet", "irc.beta.invalid"),
        "gamma" => ("GammaNick", "GammaNet", "irc.gamma.invalid"),
        "delta" => ("", "DeltaNet", "irc.delta.invalid"),
        _ => panic!("unknown test connection {id}"),
    };
    Route {
        server_id: id.into(),
        my_nick: nick.into(),
        network: network.into(),
        server: server.into(),
    }
}

fn fixture() -> (ScriptEngine, RunCtx<'static>) {
    let engine = ScriptEngine::new();
    assert_eq!(engine.assign_cid("alpha"), 1);
    assert_eq!(engine.assign_cid("removed"), 2);
    engine.forget_cid("removed");
    assert_eq!(engine.assign_cid("beta"), 3);
    assert_eq!(engine.assign_cid("gamma"), 4);
    assert_eq!(engine.assign_cid("delta"), 5);
    let states = StateStore::new();
    for (id, connected) in [("alpha", true), ("beta", true), ("gamma", false)] {
        states.set(
            id,
            StateSnapshot {
                server_id: id.into(),
                nick: route(id).my_nick,
                connect_time: if connected { 1 } else { 0 },
                ..Default::default()
            },
        );
    }
    // Delta deliberately has no snapshot: a retained CID is not a connected
    // session merely because routing supplies its server_id.
    for id in ["alpha", "beta", "gamma", "delta"] {
        let context = route(id);
        engine.set_connection_context(id, &context.network, &context.server);
    }
    engine.set_connection_states(states.clone());
    let context = RunCtx {
        server_id: "",
        my_nick: "AlphaNick",
        network: "AlphaNet",
        server: "irc.alpha.invalid",
        data_dir: std::env::temp_dir(),
        state: states.get("alpha"),
    };
    (engine, context)
}

// Inspect the application boundary: wrappers contain evaluated actions, and
// their full routing context applies recursively to those actions.
fn flatten(actions: Vec<Action>) -> Vec<(Route, Action)> {
    fn visit(actions: Vec<Action>, current: Route, output: &mut Vec<(Route, Action)>) {
        for action in actions {
            match action {
                Action::WithConnection {
                    server_id,
                    my_nick,
                    network,
                    server,
                    actions,
                } => {
                    visit(
                        actions,
                        Route {
                            server_id,
                            my_nick,
                            network,
                            server,
                        },
                        output,
                    );
                }
                action => output.push((current.clone(), action)),
            }
        }
    }
    let mut output = Vec::new();
    visit(actions, route("alpha"), &mut output);
    output
}

fn echo(text: &str) -> Action {
    Action::Echo {
        target: "(active)".into(),
        text: text.into(),
    }
}

fn feedback(text: &str) -> Action {
    Action::Echo {
        target: "(status)".into(),
        text: text.into(),
    }
}

#[test]
fn connection_selection_routes_evaluated_output_and_restores_original() {
    let (engine, context) = fixture();
    engine.load(
        "alias entry {\n\
           echo -a before=$cid/$network/$server/$me/$target/$status\n\
           scid 3\n\
           echo -a selected=$cid/$network/$server/$me/$target/$status\n\
           msg #room selected=$cid/$network/$me\n\
           scon 1\n\
           echo -a restored=$cid/$network/$me/$target\n\
         }",
    );
    assert_eq!(
        flatten(engine.run_alias(&context, "#room", "entry", "")),
        vec![
            (
                route("alpha"),
                echo("before=1/AlphaNet/irc.alpha.invalid/AlphaNick/#room/connected")
            ),
            (
                route("beta"),
                echo("selected=3/BetaNet/irc.beta.invalid/BetaNick/(status)/connected")
            ),
            (
                route("beta"),
                Action::Send("PRIVMSG #room :selected=3/BetaNet/BetaNick".into())
            ),
            (route("alpha"), echo("restored=1/AlphaNet/AlphaNick/#room")),
        ],
    );
    assert_eq!(
        flatten(engine.run_command(&context, "#fresh", "echo -a next=$cid/$target", &[])),
        vec![(route("alpha"), echo("next=1/#fresh"))],
    );
}

#[test]
fn connection_nested_alias_and_signal_restore_context_and_reset_origin() {
    let (engine, context) = fixture();
    engine.load(
        "alias entry {\n\
           scid 3\n\
           child\n\
           echo -a after-child=$cid/$network\n\
           signal -n switch payload\n\
           echo -a after-signal=$cid/$network\n\
           scid -r\n\
           echo -a origin=$cid/$network/$target\n\
         }\n\
         alias child { scid -r | echo -a child-entry=$cid | scid 4 | echo -a child=$cid/$status }\n\
         on *:SIGNAL:switch:{ scid -r | echo -a signal-entry=$cid/$1 | scid 4 | echo -a signal=$cid/$status }",
    );
    assert_eq!(
        flatten(engine.run_alias(&context, "#room", "entry", "")),
        vec![
            (route("beta"), echo("child-entry=3")),
            (route("gamma"), echo("child=4/connecting")),
            (route("beta"), echo("after-child=3/BetaNet")),
            (route("beta"), echo("signal-entry=3/payload")),
            (route("gamma"), echo("signal=4/connecting")),
            (route("beta"), echo("after-signal=3/BetaNet")),
            (route("alpha"), echo("origin=1/AlphaNet/#room")),
        ],
    );
}

#[test]
fn connection_reset_command_uses_first_origin_and_preserves_selected_context() {
    let (engine, context) = fixture();
    engine.load(
        "alias entry {\n\
           scid 3\n\
           scid 4\n\
           scon -r echo -a scoped=$cid/$network/$me\n\
           echo -a selected=$cid/$network\n\
           scid -r\n\
           echo -a restored=$cid/$network\n\
         }",
    );
    assert_eq!(
        flatten(engine.run_alias(&context, "#room", "entry", "")),
        vec![
            (route("alpha"), echo("scoped=1/AlphaNet/AlphaNick")),
            (route("gamma"), echo("selected=4/GammaNet")),
            (route("alpha"), echo("restored=1/AlphaNet")),
        ],
    );
}

#[test]
fn connection_type_filters_union_status_bits_in_stable_cid_order() {
    let (engine, context) = fixture();
    let cases: &[(u32, &[&str])] = &[
        (1, &["alpha", "beta"]),
        (2, &["gamma", "delta"]),
        (3, &["alpha", "beta", "gamma", "delta"]),
        (4, &["gamma"]),
        (5, &["alpha", "beta", "gamma"]),
        (6, &["gamma", "delta"]),
        (7, &["alpha", "beta", "gamma", "delta"]),
        (8, &["delta"]),
        (0, &["alpha", "beta", "gamma", "delta"]),
        (16, &[]),
        (17, &["alpha", "beta"]),
    ];
    for (mask, expected_ids) in cases {
        let actual = flatten(engine.run_command(
            &context,
            "#room",
            &format!("scon -at{mask} echo -a $cid/$status"),
            &[],
        ));
        let expected: Vec<_> = expected_ids
            .iter()
            .map(|id| {
                let (cid, status) = match *id {
                    "alpha" => (1, "connected"),
                    "beta" => (3, "connected"),
                    "gamma" => (4, "connecting"),
                    "delta" => (5, "disconnected"),
                    _ => unreachable!(),
                };
                (route(id), echo(&format!("{cid}/{status}")))
            })
            .collect();
        assert_eq!(actual, expected, "connection type mask {mask}");
    }
    assert_eq!(
        flatten(engine.run_command(&context, "#room", "scid -a echo -a $cid", &[])),
        vec![
            (route("alpha"), echo("1")),
            (route("beta"), echo("3")),
            (route("gamma"), echo("4")),
            (route("delta"), echo("5")),
        ],
    );
    assert!(engine
        .run_command(&context, "#room", "scid -a", &[])
        .is_empty());
}

#[test]
fn connection_type_filter_also_applies_to_one_command_destination() {
    let (engine, context) = fixture();
    assert_eq!(
        flatten(engine.run_command(&context, "#room", "scid -t4 4 echo -a selected=$cid", &[])),
        vec![(route("gamma"), echo("selected=4"))],
    );
    assert!(engine
        .run_command(&context, "#room", "scid -t4 3 echo -a excluded", &[])
        .is_empty());
    // With no command, the type mask does not prevent a context selection.
    assert_eq!(
        flatten(engine.run_command(&context, "#room", "scid -t4 3 | echo -a selected=$cid", &[])),
        vec![(route("beta"), echo("selected=3"))],
    );
}

#[test]
fn connection_selectors_distinguish_positions_cids_and_nonpositive_values() {
    let (engine, context) = fixture();
    for (command, id, cid) in [
        ("scon 2 echo -a $cid", "beta", "3"),
        ("scid 4 echo -a $cid", "gamma", "4"),
        ("scon 3 echo -a $cid", "gamma", "4"),
        ("scid 0 echo -a $cid", "alpha", "1"),
        ("scon 0 echo -a $cid", "alpha", "1"),
        ("scid - -2 echo -a $cid", "alpha", "1"),
        ("scon - -2 echo -a $cid", "alpha", "1"),
    ] {
        assert_eq!(
            flatten(engine.run_command(&context, "#room", command, &[])),
            vec![(route(id), echo(cid))],
            "selector command {command}",
        );
    }
    assert_eq!(
        flatten(engine.run_command(&context, "#room", "scid 3 | scid -s -1 | echo -a $cid", &[])),
        vec![
            (route("alpha"), feedback("* Reset connection ID to: 1")),
            (route("alpha"), echo("1")),
        ],
    );
    for name in ["scid", "scon"] {
        assert_eq!(
            flatten(engine.run_command(
                &context,
                "#room",
                &format!("{name} invalid echo -a unreachable | echo -a after=$cid"),
                &[]
            )),
            vec![(route("alpha"), echo("after=1"))],
        );
    }
}

#[test]
fn connection_feedback_uses_cids_obeys_quiet_and_keeps_errors_unconditional() {
    let (engine, context) = fixture();
    engine.load(
        "alias entry {\n\
           scid -s\n\
           scon -s 2\n\
           scon -s\n\
           scid -rs\n\
           scid -rs\n\
           .scid -s 3\n\
           .scid -s\n\
           .scid -s 99\n\
           .scon -s 99\n\
           scid -s 3 echo -a command=$cid\n\
           echo -a after=$cid\n\
         }",
    );
    assert_eq!(
        flatten(engine.run_alias(&context, "#room", "entry", "")),
        vec![
            (route("alpha"), feedback("* Active connection ID: 1")),
            (route("beta"), feedback("* Set connection ID to: 3")),
            (route("beta"), feedback("* Active connection ID: 3")),
            (route("alpha"), feedback("* Reset connection ID to: 1")),
            (route("alpha"), feedback("* Active connection ID: 1")),
            (route("beta"), feedback("* No such connection ID: 99")),
            (route("beta"), feedback("* No such connection number: 99")),
            (route("beta"), echo("command=3")),
            (route("beta"), echo("after=3")),
        ],
    );
}

#[test]
fn connection_commands_evaluate_once_on_target_and_keep_inline_routine_state() {
    let (engine, context) = fixture();
    engine.load_sources(&[
        (
            "caller.mrc".into(),
            "alias -l tag { return caller-private }\n\
          alias -l probe { inc %evaluations | return $network }\n\
          alias entry {\n\
            var %counter = 1\n\
            set %shared before\n\
            set %evaluations 0\n\
            scid 3 echo -a network=$network nick=$me probe=$probe\n\
            scid 3 echo -a params=$1/$2 source=$script private=$tag\n\
            scid 3 inc %counter\n\
            scid 3 set %shared changed\n\
            echo -a counter=%counter shared=%shared caller=$network params=$1/$2 evaluations=%evaluations\n\
          }"
            .into(),
        ),
        (
            "other.mrc".into(),
            "alias -l tag { return other-private }".into(),
        ),
    ]);
    assert_eq!(
        flatten(engine.run_alias(&context, "#room", "entry", "one two")),
        vec![
            (
                route("beta"),
                echo("network=BetaNet nick=BetaNick probe=BetaNet")
            ),
            (
                route("beta"),
                echo("params=one/two source=caller.mrc private=caller-private")
            ),
            (
                route("alpha"),
                echo("counter=2 shared=changed caller=AlphaNet params=one/two evaluations=1")
            ),
        ],
    );
    assert_eq!(
        flatten(engine.run_command(&context, "#room", "echo -a shared=%shared", &[])),
        vec![(route("alpha"), echo("shared=changed"))],
    );
}

#[test]
fn connection_single_evaluation_keeps_ordinary_and_unsafe_payloads_literal() {
    let (engine, context) = fixture();
    engine.load(
        "alias safe { scid 3 msg #room $unsafe($1-) }\n\
         alias ordinary { scid 3 msg #room $1- }",
    );
    let payload = "$me | /msg #room injected";
    assert_eq!(
        flatten(engine.run_alias(&context, "#room", "safe", payload)),
        vec![(
            route("beta"),
            Action::Send("PRIVMSG #room :$me | /msg #room injected".into())
        )],
    );
    assert_eq!(
        flatten(engine.run_alias(&context, "#room", "ordinary", payload)),
        vec![(
            route("beta"),
            Action::Send("PRIVMSG #room :$me | /msg #room injected".into())
        )],
    );
}

#[test]
fn connection_current_parseline_control_replaces_only_its_own_hook() {
    let (engine, context) = fixture();
    engine.load("on *:PARSELINE:out:*:{ scid $cid parseline -ot PRIVMSG #room :changed }");
    let outcome = dispatch_parseline(
        &engine,
        &context,
        "out",
        "PRIVMSG #room :original",
        b"PRIVMSG #room :original",
        true,
        false,
    );
    assert_eq!(outcome.current, Some(b"PRIVMSG #room :changed".to_vec()));
    assert!(outcome.actions.is_empty());
    assert!(outcome.queued.is_empty());

    // A control evaluated for another connection must stay behind that route
    // boundary and cannot replace the source connection's current wire line.
    engine.load("on *:PARSELINE:out:*:{ scid 3 parseline -ot PRIVMSG #room :other }");
    let outcome = dispatch_parseline(
        &engine,
        &context,
        "out",
        "PRIVMSG #room :original",
        b"PRIVMSG #room :original",
        true,
        false,
    );
    assert!(outcome.current.is_none());
    assert!(outcome.queued.is_empty());
    assert!(matches!(
        outcome.actions.as_slice(),
        [Action::WithConnection { .. }]
    ));
    let routed = flatten(outcome.actions);
    assert_eq!(routed.len(), 1);
    assert_eq!(routed[0].0, route("beta"));
    assert!(matches!(
        &routed[0].1,
        Action::ParseLine { direction, bytes, queue: false, .. }
            if direction == "out" && bytes == b"PRIVMSG #room :other"
    ));
}

#[test]
fn connection_current_connect_controls_remain_available_to_autojoin_extraction() {
    let (engine, context) = fixture();
    for (flags, skip, delay_secs) in [("-n", false, 0), ("-s", true, 0), ("-d5", false, 5)] {
        engine.load(&format!("on *:CONNECT:{{ scid $cid autojoin {flags} }}"));
        assert_eq!(
            engine.dispatch_event(&context, "CONNECT", EventVars::default()),
            vec![Action::Autojoin { skip, delay_secs }],
            "same-connection autojoin {flags} must remain a bare control action",
        );
    }
}
