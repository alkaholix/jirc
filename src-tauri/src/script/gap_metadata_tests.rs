use super::{
    eval::{Action, DccInfo, EventVars, ScriptDcc},
    RunCtx, ScriptEngine,
};
use std::sync::Arc;

fn context() -> RunCtx<'static> {
    let mut state = crate::irc::state::StateSnapshot::default();
    state.server_port = 6697;
    RunCtx {
        server_id: "",
        my_nick: "me",
        network: "test",
        server: "test.invalid",
        data_dir: std::env::temp_dir(),
        state: Arc::new(state),
    }
}
fn echoes(actions: Vec<Action>) -> Vec<String> {
    actions
        .into_iter()
        .filter_map(|action| match action {
            Action::Echo { text, .. } => Some(text),
            _ => None,
        })
        .collect()
}

#[test]
fn current_server_port_matches_the_current_context_port() {
    assert_eq!(
        echoes(ScriptEngine::new().run_command(
            &context(),
            "#c",
            "echo -a $port / $server(-1).port",
            &[]
        )),
        ["6697 / 6697"]
    );
}

#[test]
fn disconnect_reason_reaches_script_parameters() {
    let engine = ScriptEngine::new();
    engine.load("on *:DISCONNECT:{ echo -a reason=$1- }");
    let event = crate::irc::event::UiEvent::Disconnected {
        server_id: "s".into(),
        reason: "Closing Link: maintenance".into(),
    };
    assert_eq!(
        echoes(super::drive_event(&engine, &context(), &event)),
        ["reason=Closing Link: maintenance"]
    );
}

#[test]
fn completion_dcc_selector_uses_its_frozen_event_snapshot() {
    let engine = ScriptEngine::new();
    engine.load(
        "on *:FILERCVD:*:{ echo -a $get(-1).file $get(-1).rcvd $get(-1).done $get(-1).idle }",
    );
    let snapshot = DccInfo {
        id: "finished".into(),
        kind: "recv".into(),
        nick: "peer".into(),
        filename: "done.bin".into(),
        path: "done.bin".into(),
        status: "received".into(),
        transferred: 42,
        size: 42,
        secs: 10,
        idle: 2,
        ..Default::default()
    };
    let event = EventVars {
        nick: "peer".into(),
        filename: "done.bin".into(),
        dcc_id: "finished".into(),
        dcc_snapshot: Some(snapshot),
        ..Default::default()
    };
    assert_eq!(
        echoes(engine.dispatch_event(&context(), "FILERCVD", event)),
        ["done.bin 42 $true 2"]
    );
}

struct DccView;
impl ScriptDcc for DccView {
    fn snapshot(&self, _: &str) -> Vec<DccInfo> {
        vec![DccInfo {
            id: "chat".into(),
            kind: "chat".into(),
            nick: "peer".into(),
            status: "active".into(),
            secs: 100,
            idle: 3,
            ..Default::default()
        }]
    }
    fn server_port(&self) -> Option<u16> {
        None
    }
}
#[test]
fn dcc_idle_is_independent_from_total_duration() {
    let engine = ScriptEngine::new();
    engine.set_dcc(Arc::new(DccView));
    assert_eq!(
        echoes(engine.run_command(&context(), "#c", "echo -a $chat(1).secs $chat(1).idle", &[])),
        ["100 3"]
    );
}

#[test]
fn picture_and_editbox_flags_coexist_and_activation_preserves_picture_kind() {
    let engine = ScriptEngine::new();
    let actions = engine.run_command(&context(), "#c", "window -ep @picture", &[]);
    assert!(
        matches!(&actions[0], Action::WindowOpen { kind, editbox: true, .. } if kind == "picture")
    );
    let actions = engine.run_command(&context(), "#c", "window -a @picture", &[]);
    assert!(
        matches!(&actions[0], Action::WindowOpen { kind, editbox: true, activate: true, .. } if kind == "picture")
    );
}

fn dialogs(engine: &ScriptEngine) {
    engine.load("dialog table {\n title \"Test\"\n size -1 -1 240 180\n edit \"Old\", 10, 1 1 100 20\n text \"Caption\", 11, 1 25 100 20\n list 20, 1 50 100 60, multsel\n combo 21, 1 115 100 30, drop\n}\nalias prepare { dialog -m A table | dialog -m B table | did -ra A 10 Alpha | did -ra B 10 Beta }\non *:DIALOG:A:sclick:10:{ echo -a other=$did(B,10) | did -ra A 10 Fresh | echo -a now=$did(A,10) }");
    engine.run_alias(&context(), "#c", "prepare", "");
}

#[test]
fn named_dialog_reads_work_outside_events_and_after_same_run_mutation() {
    let engine = ScriptEngine::new();
    dialogs(&engine);
    assert_eq!(
        echoes(engine.run_command(
            &context(),
            "#c",
            "echo -a $did(A,10) / $did(B,10) | did -ra A 10 New | echo -a $did(A,10) / $did(B,10)",
            &[]
        )),
        ["Alpha / Beta", "New / Beta"]
    );
}

#[test]
fn dialog_user_snapshot_does_not_override_other_dialog_or_later_script_changes() {
    let engine = ScriptEngine::new();
    dialogs(&engine);
    let vars = EventVars {
        dialog_name: "A".into(),
        dialog_event: "sclick".into(),
        dialog_control: "10".into(),
        text: "10".into(),
        params: vec!["10".into()],
        did: [("10".into(), "Typed".into())].into(),
        ..Default::default()
    };
    assert_eq!(
        echoes(engine.dispatch_event(&context(), "DIALOG", vars)),
        ["other=Beta", "now=Fresh"]
    );
    assert_eq!(
        echoes(engine.run_command(&context(), "#c", "echo -a $did(A,10)", &[])),
        ["Fresh"]
    );
}

#[test]
fn late_dialog_creation_snapshot_does_not_restore_defaults_over_script_values() {
    let engine = ScriptEngine::new();
    dialogs(&engine);
    engine.dispatch_event(
        &context(),
        "DIALOG",
        EventVars {
            dialog_name: "A".into(),
            dialog_event: "init".into(),
            dialog_control: "0".into(),
            did: [("10".into(), "Old".into())].into(),
            ..Default::default()
        },
    );
    assert_eq!(
        echoes(engine.run_command(&context(), "#c", "echo -a $did(A,10)", &[])),
        ["Alpha"]
    );
}

#[test]
fn dialog_add_reset_selection_and_multiselection_are_synchronous() {
    let engine = ScriptEngine::new();
    dialogs(&engine);
    let command = "did -a A 10 Tail | did -ra A 11 Replaced | did -a A 20 one | did -a A 20 two | did -a A 20 three | did -c A 20 1 | did -ck A 20 3 | did -a A 21 one | did -a A 21 two | did -c A 21 2 | echo -a $did(A,10) / $did(A,11) / $did(A,20,0).sel / $did(A,20,1).sel / $did(A,20,2).sel / $did(A,21).seltext | did -ra A 20 single | echo -a lines=$did(A,20).lines";
    assert_eq!(
        echoes(engine.run_command(&context(), "#c", command, &[])),
        ["AlphaTail / Replaced / 2 / 1 / 3 / two", "lines=1"]
    );
}

#[test]
fn hotlink_distinguishes_raw_word_normalized_match_and_character_position() {
    let engine = ScriptEngine::new();
    engine.load("on *:HOTLINK:*:*:{ echo -a $hotlink(word) / $hotlink(match) / $hotlink(word).pos / $hotlink(match).pos }");
    let mut ctx = context();
    let mut state = (*ctx.state).clone();
    state.ial = vec![("Alice".into(), "Alice!u@host".into())];
    ctx.state = Arc::new(state);
    let event = EventVars {
        text: "<@Alice>".into(),
        params: vec!["<@Alice>".into()],
        hotlink_pos: 3,
        hotlink_char_pos: 8,
        hotlink_line_text: "one two <@Alice>".into(),
        ..Default::default()
    };
    assert_eq!(
        echoes(engine.dispatch_event(&ctx, "HOTLINK", event)),
        ["<@Alice> / Alice / 3 / 10"]
    );
}
