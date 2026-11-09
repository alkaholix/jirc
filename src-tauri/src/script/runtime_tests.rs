use super::{
    eval::{Action, EventVars},
    RunCtx, ScriptEngine,
};

fn context() -> RunCtx<'static> {
    RunCtx {
        server_id: "",
        my_nick: "me",
        network: "test",
        server: "test.invalid",
        data_dir: std::env::temp_dir(),
        state: Default::default(),
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
fn required_identifier_arguments_enter_error_label_and_reset() {
    let engine = ScriptEngine::new();
    engine.load("alias probe { echo -a before | echo -a $mid(1) | echo -a AFTER | return | :error | echo -a caught=$error | reseterror | echo -a reset=$error }");
    let output = echoes(engine.run_alias(&context(), "#c", "probe", ""));
    assert_eq!(output[0], "before");
    assert!(
        output[1].contains("caught=* Invalid parameters: $mid"),
        "{output:?}"
    );
    assert_eq!(output[2], "reset=");
    assert_eq!(output.len(), 3);
}

#[test]
fn identifier_failure_unwinds_to_callers_error_handler() {
    let engine = ScriptEngine::new();
    engine.load("alias bad { echo -a bad-before | echo -a $left(x) | echo -a BAD-AFTER }\nalias outer { bad | echo -a OUTER-AFTER | return | :error | echo -a outer-caught | reseterror }");
    assert_eq!(
        echoes(engine.run_alias(&context(), "#c", "outer", "")),
        ["bad-before", "outer-caught"]
    );
}

#[test]
fn returning_without_reset_propagates_a_caught_error_to_caller() {
    let engine = ScriptEngine::new();
    engine.load("alias bad { echo -a $mid(x) | return | :error | echo -a inner-caught | return }\nalias outer { bad | echo -a WRONG | return | :error | echo -a outer-caught | reseterror }");
    assert_eq!(
        echoes(engine.run_alias(&context(), "#c", "outer", "")),
        ["inner-caught", "outer-caught"]
    );
}

#[test]
fn error_escapes_nested_blocks_without_executing_later_statements() {
    let engine = ScriptEngine::new();
    engine.load("alias probe { while (1) { if (1) { echo -a $mid(x) } | echo -a WRONG } | return | :error | echo -a caught | reseterror }");
    assert_eq!(
        echoes(engine.run_alias(&context(), "#c", "probe", "")),
        ["caught"]
    );
}

#[test]
fn a_failed_assignment_preserves_the_previous_variable_value() {
    let engine = ScriptEngine::new();
    engine.load("alias probe { var %keep = old | set %keep $mid(x) | return | :error | echo -a keep=%keep | reseterror }");
    assert_eq!(
        echoes(engine.run_alias(&context(), "#c", "probe", "")),
        ["keep=old"]
    );
}

#[test]
fn a_failed_file_expression_does_not_write_a_partial_line() {
    let engine = ScriptEngine::new();
    let path = std::env::temp_dir().join(format!("jirc-error-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&path).unwrap();
    let mut ctx = context();
    ctx.data_dir = path.clone();
    engine.load("alias probe { write result.txt $mid(x) | return | :error | reseterror }");
    engine.run_alias(&ctx, "#c", "probe", "");
    assert!(!path.join("result.txt").exists());
    std::fs::remove_dir(path).unwrap();
}

#[test]
fn failed_statement_abandons_queued_http_registration() {
    let engine = ScriptEngine::new();
    engine.load("alias probe { echo -a $urlget(http://127.0.0.1:9/x,gb,&out,done) $mid(x) | return | :error | echo -a remaining=$urlget(0) | reseterror }");
    let actions = engine.run_alias(&context(), "#c", "probe", "");
    assert!(actions
        .iter()
        .all(|action| !matches!(action, Action::UrlGet { .. })));
    assert_eq!(echoes(actions), ["remaining=0"]);
}

#[test]
fn default_level_is_clamped_and_used_for_unlisted_event_senders() {
    let engine = ScriptEngine::new();
    engine.load("on 5:TEXT:probe:#:{ echo -a level=$ulevel }");
    engine.run_command(&context(), "#c", "dlevel 5", &[]);
    let actions = engine.dispatch_event(
        &context(),
        "TEXT",
        EventVars {
            nick: "guest".into(),
            chan: "#c".into(),
            target: "#c".into(),
            text: "probe".into(),
            params: vec!["probe".into()],
            ..Default::default()
        },
    );
    assert_eq!(echoes(actions), ["level=5"]);
    for (value, expected) in [("2000", "999"), ("-5", "1"), ("bogus", "1")] {
        engine.run_command(&context(), "#c", &format!("dlevel {value}"), &[]);
        assert_eq!(
            echoes(engine.run_command(&context(), "#c", "echo -a $dlevel", &[])),
            [expected]
        );
    }
}

#[test]
fn normal_halt_marks_later_files_without_hiding_default_display() {
    let engine = ScriptEngine::new();
    engine.load_sources(&[
        (
            "a.mrc".into(),
            "on *:START:{ echo -a first | halt | echo -a WRONG }".into(),
        ),
        ("b.mrc".into(), "on &*:START:{ echo -a WRONG }".into()),
        (
            "c.mrc".into(),
            "on *:START:{ echo -a halted=$halted }".into(),
        ),
    ]);
    let (actions, halted, default_halted) =
        engine.dispatch_event_status(&context(), "START", EventVars::default(), None, None);
    assert_eq!(echoes(actions), ["first", "halted=$true"]);
    assert!(halted);
    assert!(!default_halted);
}
