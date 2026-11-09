use super::{eval::Action, RunCtx, ScriptEngine};

fn evaluate(expression: &str) -> String {
    let context = RunCtx {
        server_id: "test",
        my_nick: "me",
        network: "test",
        server: "test.invalid",
        data_dir: std::env::temp_dir(),
        state: Default::default(),
    };
    let actions = ScriptEngine::new().run_command(
        &context,
        "(status)",
        &format!("echo -s output= $+ [ $+ {expression} $+ ]"),
        &[],
    );
    let [Action::Echo { text, .. }] = actions.as_slice() else {
        panic!("Expected one echo for {expression}: {actions:?}");
    };
    text.strip_prefix("output=[")
        .unwrap()
        .strip_suffix(']')
        .unwrap()
        .into()
}

#[test]
fn quote_helpers_handle_each_edge_independently() {
    for (expression, expected) in [
        ("$qt(abc)", "\"abc\""),
        ("$qt(a b)", "\"a b\""),
        ("$qt($null)", "\"\""),
        ("$qt($chr(34) $+ abc)", "\"abc\""),
        ("$qt(abc $+ $chr(34))", "\"abc\""),
        ("$qt($chr(34) $+ abc $+ $chr(34))", "\"abc\""),
        ("$qt($chr(34))", "\""),
        ("$noqt($chr(34) $+ abc)", "abc"),
        ("$noqt(abc $+ $chr(34))", "abc"),
        ("$noqt($chr(34) $+ abc $+ $chr(34))", "abc"),
        ("$noqt($chr(34))", ""),
        ("$noqt($null)", ""),
        (
            "$noqt($chr(34) $+ $chr(32) $+ abc $+ $chr(32) $+ $chr(34))",
            "abc",
        ),
        ("$noqt(abc $+ $chr(32))", "abc"),
        ("$qt(a $+ $chr(34) $+ b)", "\"a\"b\""),
        ("$noqt(a $+ $chr(34) $+ b)", "a\"b"),
    ] {
        assert_eq!(evaluate(expression), expected, "{expression}");
    }
}

#[test]
fn quoting_keeps_generated_identifier_and_pipe_text_as_data() {
    assert_eq!(
        evaluate("$qt($!me $chr(124) msg #bad text)"),
        "\"$me | msg #bad text\""
    );
}

#[test]
fn byte_format_options_and_suffix_are_wired_through_identifiers() {
    for (expression, expected) in [
        ("$bytes(1024,k)", "1"),
        ("$bytes(1048576,m)", "1"),
        ("$bytes(1024,b)", "1,024"),
        ("$bytes(1536,kd)", "1.5"),
        ("$bytes(1024)", "1"),
        ("$bytes(1024,k).suf", "1KB"),
        ("$bytes(-1536,kd).suf", "-1.5KB"),
        ("$bytes(11.35,3d)", "11.3"),
    ] {
        assert_eq!(evaluate(expression), expected, "{expression}");
    }
}

#[test]
fn quote_helpers_integrate_with_sandboxed_file_and_ini_commands() {
    let directory =
        std::env::temp_dir().join(format!("jirc-quoted-files-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).unwrap();
    let context = RunCtx {
        server_id: "local",
        my_nick: "me",
        network: "Local",
        server: "",
        data_dir: directory.clone(),
        state: Default::default(),
    };
    let engine = ScriptEngine::new();
    engine.load("alias check {\n\
      fopen -o h $qt(payload.bin)\n\
      fwrite h ABC\n\
      fclose h\n\
      writeini -n $qt(settings.ini) state item value\n\
      writeini -n $qt(settings with spaces.ini) state item spaced\n\
      echo -s plain= $+ $readini($qt(settings.ini),n,state,item) spaced= $+ $readini($qt(settings with spaces.ini),n,state,item)\n\
      remini $qt(settings with spaces.ini) state item\n\
      echo -s removed= $+ [ $+ $readini($qt(settings with spaces.ini),n,state,item) $+ ]\n\
    }");
    let actions = engine.run_alias(&context, "(status)", "check", "");
    let values: Vec<_> = actions
        .into_iter()
        .filter_map(|action| match action {
            Action::Echo { text, .. } => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(values, ["plain=value spaced=spaced", "removed=[]"]);
    assert_eq!(
        std::fs::read(directory.join("payload.bin")).unwrap(),
        b"ABC"
    );
    for name in ["payload.bin", "settings.ini", "settings with spaces.ini"] {
        std::fs::remove_file(directory.join(name)).unwrap();
    }
    std::fs::remove_dir(directory).unwrap();
}
