//! Parsing rules pinned against native mIRC 7.79 (TODO P-01, P-02, P-03, P-06).
//! Expected values are what mIRC printed for the same script lines; see
//! `analysis/rea-batch1-20261109/`.

use super::{eval::Action, RunCtx, ScriptEngine};
use crate::irc::state::StateSnapshot;
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

fn echoes(actions: &[Action]) -> Vec<String> {
    actions
        .iter()
        .filter_map(|action| match action {
            Action::Echo { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// Runs `body` as alias `t` and returns what `out` printed, `<…>`-wrapped so
/// leading and trailing spaces stay visible.
fn run(extra: &str, body: &str) -> Vec<String> {
    let engine = ScriptEngine::new();
    engine.load(&format!(
        "alias out {{ echo -a < $+ $1- $+ > }}\n{extra}\nalias t {{\n  {body}\n}}"
    ));
    echoes(&engine.run_alias(&context(), "#c", "t", ""))
}

#[test]
fn windows_only_interfaces_are_rejected_locally() {
    for name in [
        "dll",
        "dde",
        "ddeserver",
        "comopen",
        "comclose",
        "comlist",
        "comreg",
    ] {
        for line in [
            name.to_string(),
            format!("{name} a b c"),
            format!("!{name} a b"),
        ] {
            let actions = ScriptEngine::new().run_command(&context(), "#c", &line, &[]);
            assert!(
                !actions
                    .iter()
                    .any(|action| matches!(action, Action::Send(_))),
                "/{line} reached the server: {actions:?}"
            );
            let printed = echoes(&actions);
            assert_eq!(printed.len(), 1, "/{line}: {printed:?}");
            assert!(
                printed[0].starts_with(&format!("* /{name}: not supported in jIRC")),
                "/{line}: {printed:?}"
            );
        }
    }
}

#[test]
fn windows_only_interface_stops_the_script() {
    let printed = run("", "out before | dll x.dll f | out after");
    assert_eq!(printed.len(), 2, "{printed:?}");
    assert_eq!(printed[0], "<before>");
    assert!(
        printed[1].starts_with("* /dll: not supported in jIRC"),
        "{printed:?}"
    );
}

#[test]
fn require_prefix_with_a_value_passes_it_through() {
    assert_eq!(run("", "tokenize 32 a | out x $$1 y"), ["<x a y>"]);
    assert_eq!(run("", "tokenize 32 a b | out x $$1- y"), ["<x a b y>"]);
}

#[test]
fn empty_require_prefix_halts_without_running_the_command() {
    for body in [
        "tokenize 32 a | out x $$2 y",
        "tokenize 32 a | out $upper($$2) z",
        "tokenize 32 a | if ($$2 == x) out yes | out after-if",
        "tokenize 32 a | var %x $$2 | out after-var %x",
        "out $$null z",
        "tokenize 32 a | out x $$2 y | out second",
        "tokenize 32 a | out x $$upper($2) y",
    ] {
        assert_eq!(run("", body), Vec::<String>::new(), "{body}");
    }
}

#[test]
fn empty_require_prefix_in_a_called_alias_halts_the_caller() {
    let callee = "alias callee {\n  tokenize 32 a | out in $$2\n}";
    assert_eq!(
        run(callee, "out before | callee | out after-caller"),
        ["<before>"]
    );
    assert_eq!(
        run(callee, "out before\n  callee\n  out after-caller"),
        ["<before>"]
    );
}

#[test]
fn built_variable_names_join_with_dollar_plus() {
    for (body, expected) in [
        (
            "var %pre hello | var %pre $+ fix 7 | out A %prefix B %hellofix",
            "<A 7 B>",
        ),
        ("set %n. $+ $upper(ab) 3 | out %n.AB", "<3>"),
        ("inc %c. $+ a | inc %c. $+ a | out %c.a", "<2>"),
        ("set %d. $+ a 5 | dec %d. $+ a | out %d.a", "<4>"),
        (
            "set %u. $+ a 5 | unset %u. $+ a | out X $+ %u.a $+ X",
            "<XX>",
        ),
        ("var %e. $+ b = 5 | out %e.b", "<= 5>"),
        ("set -n %t $+ x 1+1 | out %tx", "<1+1>"),
        ("var %a 1 , %b. $+ c 2 | out %a %b.c", "<1 2>"),
        ("set %z $+ $null $+ q 4 | out %zq", "<4>"),
        (
            "set %y1 abc | set %y1 $+ 2 9 | out %y1 %y12 %abc2",
            "<abc 9>",
        ),
        ("set %k abc | inc %k $+ 1 | out %k1 %abc1", "<1>"),
    ] {
        assert_eq!(run("", body), [expected], "{body}");
    }
}

#[test]
fn text_after_a_word_start_identifier_call_is_dropped() {
    for (body, expected) in [
        ("out $len(abc)) x", "<3 x>"),
        ("out a) b", "<a) b>"),
        ("out $upper(ab))) z", "<AB z>"),
        ("out (Count: $calc(1+1))", "<(Count: 2>"),
        ("out ( $len(abc) )", "<( 3 )>"),
        ("out $len(abc)) $+ x", "<3x>"),
        ("out $upper(ab)x) z", "<AB z>"),
        ("out $calc((1+2)*3))", "<9>"),
        ("out $upper(ab) ) z", "<AB ) z>"),
        ("var %v $len(abcd)) | out %v", "<4>"),
        ("out $iif(1,yes,no)) z", "<yes z>"),
        ("out $upper(ab)x", "<AB>"),
        ("out $upper(ab)x)y", "<AB>"),
        ("out $upper(ab)$lower(CD)", "<AB>"),
        ("out $upper(a)-$upper(b)", "<A>"),
        ("out $len(a(b)c)", "<5>"),
        ("out $upper(ab)).", "<AB>"),
        ("out $upper(ab),x)", "<AB>"),
        ("out $upper(ab)x)y)z", "<AB>"),
        ("out $upper(ab) $+ x)", "<ABx)>"),
        ("out $upper($lower(AB)))", "<AB>"),
        ("var %w xy | out $upper(%w))", "<XY>"),
        ("tokenize 32 q | out $upper($1)x)", "<Q>"),
        ("out $calc(2*(3+4))x", "<14>"),
    ] {
        assert_eq!(run("", body), [expected], "{body}");
    }
}

/// Runs `first` as alias `t1`, then `second` as alias `t2` on the same
/// engine, and returns what `t2` printed.
fn run_then(first: &str, second: &str) -> Vec<String> {
    let engine = ScriptEngine::new();
    engine.load(&format!(
        "alias out {{ echo -a < $+ $1- $+ > }}\nalias t1 {{\n  {first}\n}}\nalias t2 {{\n  {second}\n}}"
    ));
    engine.run_alias(&context(), "#c", "t1", "");
    echoes(&engine.run_alias(&context(), "#c", "t2", ""))
}

#[test]
fn text_after_a_call_inside_calc_arguments_is_kept() {
    for (body, expected) in [
        ("out $calc($len(abcd)-1)", "<3>"),
        ("out $calc($len(abc)*2)", "<6>"),
        (
            "bset -t &abc 1 abcdefghi | out $calc($bvar(&abc,4)*256+$bvar(&abc,3))",
            "<25699>",
        ),
        ("out $left(abcdef,$calc($len(abcdef)-2))", "<abcd>"),
        ("out $gettok(a.b.c,$calc($numtok(a.b.c,46)-1),46)", "<b>"),
        ("var %x $calc($len(abc)+1) | out %x", "<4>"),
        ("out $mid(abcdef,$calc($pos(abcdef,c)+1),2)", "<de>"),
        ("out $calc($len(ab)+$len(abc))", "<5>"),
        ("out $calc(($len(abcd)-1)*2)", "<6>"),
        ("out x $calc($len(abcd)-1) y", "<x 3 y>"),
        ("out $calc($len(abcd)-1)z", "<3>"),
        ("out $round($calc($len(abc)/2),0)", "<2>"),
    ] {
        assert_eq!(run("", body), [expected], "{body}");
    }
}

#[test]
fn text_after_a_call_inside_other_identifier_arguments_is_dropped() {
    for (body, expected) in [
        ("out $upper($lower(AB)x)", "<AB>"),
        ("out $+($upper(a)b,c)", "<Ac>"),
        ("out $len($upper(ab)cd)", "<2>"),
        ("out $len($upper(ab)-cd)", "<2>"),
        ("out $len($upper(ab)*cd)", "<2>"),
        ("out $len($upper(ab)$lower(CD))", "<2>"),
        ("out $replace($upper(ab)-x,-,+)", "<AB>"),
        ("out $upper($lower(AB)-x)", "<AB>"),
        ("out $str(x,$len(ab)) y", "<xx y>"),
        ("out x $+ $upper(ab)y", "<xAB>"),
        ("out $+(a,b)c", "<ab>"),
        ("out $($upper(a),1)x", "<A>"),
        ("if ($len(abc)x == 3) out eq | else out ne", "<eq>"),
    ] {
        assert_eq!(run("", body), [expected], "{body}");
    }
}

#[test]
fn require_halt_through_goto_keeps_the_callee_output() {
    let callee = "alias callee { out first | tokenize 32 a | goto $$2 }";
    assert_eq!(run(callee, "callee | out after"), ["<first>"]);
    let callee = "alias callee { out first | tokenize 32 a | out in $$2 }";
    assert_eq!(run(callee, "callee | out after"), ["<first>"]);
}

#[test]
fn require_halt_is_not_cleared_by_an_identifier_alias_with_if_or_while() {
    let aliases =
        "alias g2 { if (1) return y }\nalias g3 { var %i 0 | while (%i < 1) inc %i | return z }";
    for body in [
        "tokenize 32 a | msg #c x $$2 $g2",
        "tokenize 32 a | msg #c x $g2 $$2",
        "tokenize 32 a | msg #c x $$2 $g3",
    ] {
        let engine = ScriptEngine::new();
        engine.load(&format!("{aliases}\nalias t {{\n  {body}\n}}"));
        assert_eq!(
            engine.run_alias(&context(), "#c", "t", ""),
            vec![],
            "{body}"
        );
    }
    assert_eq!(
        run(aliases, "tokenize 32 a | out x $$2 $g3 | out after"),
        Vec::<String>::new()
    );
}

#[test]
fn empty_require_prefix_changes_no_state() {
    assert_eq!(
        run_then("set %gb old | tokenize 32 a | %gb = $$2", "out %gb"),
        ["<old>"]
    );
    assert_eq!(
        run_then("set %gc old | tokenize 32 a | %gc = x $+ $$2", "out %gc"),
        ["<old>"]
    );
    assert_eq!(
        run_then("set %ic 1 | tokenize 32 a | inc %ic $$2", "out %ic"),
        ["<1>"]
    );
    assert_eq!(
        run_then(
            "hmake vt 10 | tokenize 32 a | hadd vt k $$2",
            "out n= $+ $hget(vt,0).item"
        ),
        ["<n=0>"]
    );
    assert_eq!(
        run_then("set %fu 1 | tokenize 32 a | unset %fu $$2", "out %fu"),
        ["<1>"]
    );
    assert_eq!(
        run_then(
            "set %fv 1 | set %fw 2 | tokenize 32 a b | unset %fv $$2 | unset %fw",
            "out fv= $+ %fv fw= $+ %fw"
        ),
        ["<fv= fw=>"]
    );
    assert_eq!(
        run_then(
            "hmake gh 10 | hadd gh k v | tokenize 32 a | hdel gh k $$2",
            "out n= $+ $hget(gh,0).item"
        ),
        ["<n=1>"]
    );
    assert_eq!(
        run_then("tokenize 32 a | hmake gm $$2", "out t= $+ $hget(gm)"),
        ["<t=>"]
    );
    let kept = std::env::temp_dir().join(format!("jirc-require-rm-{}.txt", std::process::id()));
    std::fs::write(&kept, "x").unwrap();
    run(
        "",
        &format!(
            "tokenize 32 a | remove {} $$2",
            kept.file_name().unwrap().to_str().unwrap()
        ),
    );
    assert!(kept.exists(), "remove ran despite an empty $$2");
    let _ = std::fs::remove_file(&kept);
    let file = format!("jirc-require-{}.txt", std::process::id());
    let path = std::env::temp_dir().join(&file);
    let _ = std::fs::remove_file(&path);
    run("", &format!("tokenize 32 a | write {file} $$2"));
    assert!(!path.exists(), "write ran despite an empty $$2");
}

#[test]
fn a_halt_inside_an_identifier_alias_stops_the_calling_command() {
    for (aliases, body, expected) in [
        (
            "alias f { tokenize 32 a | return $$2 }",
            "out x $f y | out after",
            vec![],
        ),
        ("alias fh { halt }", "out x $fh y | out after", vec![]),
        (
            "alias fe { out inside | halt }",
            "out x $fe y | out after",
            vec!["<inside>"],
        ),
        (
            "alias fq { out inside | tokenize 32 a | return $$2 }",
            "out x $fq y | out after",
            vec!["<inside>"],
        ),
        (
            "alias g { tokenize 32 a | if ($$2) return x }",
            "out v $g w | out after",
            vec![],
        ),
    ] {
        assert_eq!(run(aliases, body), expected, "{aliases} / {body}");
    }
}

#[test]
fn each_popup_label_is_evaluated_on_its_own() {
    // Native mIRC 7.79 status menu (verifier pin): the empty-`$$1` item and the
    // halting `$vhl` item are hidden; every other item still shows.
    let engine = ScriptEngine::new();
    engine.load(
        "alias vhl { halt }\n\
         alias vlbl { return Custom1 }\n\
         alias vlbl2 { return Custom2 }\n\
         alias vsm { if ($1 isnum) return $iif($1 < 3,Item $1:noop) }\n\
         menu status {\n  Whois $$1:noop\n  $vlbl:noop\n  $vhl:noop\n  $vlbl2:noop\n  $submenu($vsm($1))\n  Plain:noop\n}",
    );
    let labels: Vec<String> = engine
        .popups_evaluated(&context(), "status", "", "")
        .into_iter()
        .map(|item| item.label)
        .collect();
    assert_eq!(labels, ["Custom1", "Custom2", "Item 1", "Item 2", "Plain"]);
}
