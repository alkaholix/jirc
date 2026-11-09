use super::{eval::Action, RunCtx, ScriptEngine};
use crate::irc::state::{StateSnapshot, StateStore};
use std::sync::Arc;

fn context() -> RunCtx<'static> {
    RunCtx {
        server_id: "alpha",
        my_nick: "me",
        network: "Alpha",
        server: "alpha.invalid",
        data_dir: std::env::temp_dir(),
        state: Arc::new(StateSnapshot {
            server_id: "alpha".into(),
            ..Default::default()
        }),
    }
}

fn additions(actions: &[Action]) -> Vec<(&str, &str)> {
    actions
        .iter()
        .filter_map(|action| match action {
            Action::WindowLine { name, op, text, .. } if op == "add" => {
                Some((name.as_str(), text.as_str()))
            }
            _ => None,
        })
        .collect()
}

fn status(text: &str) -> Action {
    Action::Echo {
        target: "(status)".into(),
        text: text.into(),
    }
}

#[test]
fn echo_active_new_custom_window_is_readable_immediately_and_after_the_run() {
    for flags in ["", "-e", "-ep"] {
        let engine = ScriptEngine::new();
        engine.load(&format!(
            "alias check {{ window {flags} @new | echo -a hello | echo -s $line(@new,0) / $line(@new,1) }}"
        ));
        let actions = engine.run_alias(&context(), "#caller", "check", "");
        assert_eq!(additions(&actions), [("@new", "hello")], "{flags}");
        assert_eq!(actions.last(), Some(&status("1 / hello")));
        assert!(!actions.iter().any(|action| matches!(action,
            Action::Echo { target, .. } if target == "(active)")));
        assert_eq!(
            engine.run_command(&context(), "#caller", "echo -s $line(@new,1)", &[]),
            [status("hello")]
        );
    }
}

#[test]
fn echo_active_custom_window_preserves_literal_text_colour_and_quiet_calls() {
    let engine = ScriptEngine::new();
    engine.run_command(&context(), "#caller", "window @Existing", &[]);
    engine.window_open("alpha", "@Existing");
    engine.set_active_win("alpha", "@existing");
    engine.load(
        "alias quiet { echo -qa suppressed }\n\
         alias check { .quiet | var %payload = $!me $chr(124) msg #bad text | echo 4 -a %payload | echo -a $chr(9) | echo -a | echo -s $line(@Existing,0) }",
    );
    let actions = engine.run_alias(&context(), "#caller", "check", "");
    assert_eq!(
        additions(&actions),
        [
            ("@Existing", "\u{3}04$me | msg #bad text"),
            ("@Existing", "\t")
        ]
    );
    assert_eq!(actions.last(), Some(&status("2")));
    assert_eq!(actions.len(), 3);
}

#[test]
fn echo_active_follows_explicit_activation_and_close_fallback_in_one_run() {
    let engine = ScriptEngine::new();
    engine.window_open("alpha", "Status Window");
    engine.set_active_win("alpha", "Status Window");
    engine.load(
        "alias check {\n\
           window @one\n\
           window @two\n\
           window @one\n\
           echo -a second\n\
           window -a @one\n\
           echo -a first\n\
           window -c @one\n\
           echo -a third\n\
           echo -s $line(@one,0) / $line(@two,0) / $line(@two,1) / $line(@two,2)\n\
           window -c @two\n\
           echo -sa status-only\n\
         }",
    );
    let actions = engine.run_alias(&context(), "#caller", "check", "");
    assert_eq!(
        additions(&actions),
        [("@two", "second"), ("@one", "first"), ("@two", "third")]
    );
    assert!(actions.contains(&status("0 / 2 / second / third")));
    assert_eq!(actions.last(), Some(&status("status-only")));
    assert_eq!(
        actions
            .iter()
            .filter(|a| **a == status("status-only"))
            .count(),
        1
    );
    assert!(!actions.iter().any(|action| matches!(action,
        Action::Echo { target, .. } if target == "(active)")));
}

#[test]
fn echo_active_combined_status_and_custom_outputs_are_each_emitted_once() {
    let engine = ScriptEngine::new();
    engine.load("alias check { window @log | echo -sa hello | echo -s $line(@log,0) }");
    let actions = engine.run_alias(&context(), "#caller", "check", "");
    assert_eq!(additions(&actions), [("@log", "hello")]);
    assert_eq!(&actions[1..2], [status("hello")]);
    assert_eq!(actions.last(), Some(&status("1")));
    assert_eq!(actions.len(), 4);
    for name in ["Status Window", "(status)"] {
        let engine = ScriptEngine::new();
        engine.window_open("alpha", name);
        engine.set_active_win("alpha", name);
        assert_eq!(
            engine.run_command(&context(), "#caller", "echo -sa hello", &[]),
            [status("hello")]
        );
    }
}

#[test]
fn echo_active_on_another_connection_preserves_status_fallback_without_mutation() {
    let engine = ScriptEngine::new();
    engine.run_command(&context(), "#caller", "window @log", &[]);
    engine.window_open("beta", "@log");
    engine.set_active_win("beta", "@log");
    assert_eq!(
        engine.run_command(&context(), "#caller", "echo -a hello", &[]),
        [Action::Echo {
            target: "(active)".into(),
            text: "hello".into()
        }]
    );
    assert_eq!(
        engine.run_command(&context(), "#caller", "echo -sa once", &[]),
        [status("once")]
    );
    assert_eq!(
        engine.run_command(&context(), "#caller", "echo -s $line(@log,0)", &[]),
        [status("0")]
    );
}

#[test]
fn echo_active_custom_window_uses_the_selected_connection_action_route() {
    let engine = ScriptEngine::new();
    engine.assign_cid("alpha");
    engine.assign_cid("beta");
    let states = StateStore::new();
    states.set("alpha", (*context().state).clone());
    states.set(
        "beta",
        StateSnapshot {
            server_id: "beta".into(),
            ..Default::default()
        },
    );
    engine.set_connection_states(states);
    engine.set_connection_context("beta", "Beta", "beta.invalid");
    engine.run_command(&context(), "#caller", "window @log", &[]);
    engine.window_open("beta", "@log");
    engine.set_active_win("beta", "@log");
    engine.load("alias check { scid 2 | echo -a routed | echo -s $line(@log,1) }");
    let actions = engine.run_alias(&context(), "#caller", "check", "");
    let [Action::WithConnection {
        server_id, actions, ..
    }] = actions.as_slice()
    else {
        panic!("Expected one destination action group: {actions:?}");
    };
    assert_eq!(server_id, "beta");
    assert_eq!(additions(actions), [("@log", "routed")]);
    assert_eq!(actions.last(), Some(&status("routed")));
}

#[test]
fn echo_active_local_console_keeps_ownership_between_commands_without_protocol_state() {
    let engine = ScriptEngine::new();
    let context = RunCtx {
        server_id: "local",
        state: Arc::new(StateSnapshot::default()),
        ..context()
    };
    engine.run_command(&context, "(status)", "window @local", &[]);
    engine.window_open("local", "@local");
    engine.set_active_win("local", "@local");
    for text in ["first", "second"] {
        let actions = engine.run_command(&context, "@local", &format!("echo -a {text}"), &[]);
        assert_eq!(additions(&actions), [("@local", text)]);
    }
    assert_eq!(
        engine.run_command(
            &context,
            "@local",
            "echo -s $line(@local,0) / $line(@local,1) / $line(@local,2) / $status",
            &[]
        ),
        [status("2 / first / second / disconnected")]
    );
}
