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

// ---- Batch 2: word-start evaluation (P-04) and `$*` (P-05), mIRC 7.79 pins
// in analysis/rea-batch2-20261109/ (case IDs in comments).

fn check(cases: &[(&str, &str)]) {
    for (body, expected) in cases {
        assert_eq!(run("", body), [*expected], "{body}");
    }
}

#[test]
fn identifiers_and_variables_evaluate_only_at_the_start_of_a_word() {
    check(&[
        (
            "tokenize 32 X Y | set %y Y | out a$1b [$1] x%y $1 %y",
            "<a$1b [$1] x%y X Y>",
        ), // p01
        ("tokenize 32 Khaled xyz | out $1$2", "<Khaled$2>"), // p02
        ("tokenize 32 X | out $upper(a$1)", "<A$1>"),        // p03
        ("set %v Q | out $upper(x%v)", "<X%V>"),             // p04
        ("tokenize 32 X | out $upper(a $1)", "<A X>"),       // p05
        ("tokenize 32 X | out a $+ $1", "<aX>"),             // p06
        ("tokenize 32 X Y | out x$1-", "<x$1->"),            // p10
        ("set %x Q | out (%x)", "<(%x)>"),                   // p13
        ("tokenize 32 X | out \"$1\"", "<\"$1\">"),          // p14
        ("tokenize 32 X | out nick!$1@host", "<nick!$1@host>"), // p15
        ("tokenize 32 X | out -$1", "<-$1>"),                // p16
        ("tokenize 32 X | out @$1", "<@$1>"),                // p17
        ("tokenize 32 X | out $1$1", "<X$1>"),               // p20
        ("tokenize 32 X | out $+(a,$1)", "<aX>"),            // p22
        ("tokenize 32 X | out $+(a$1,b)", "<a$1b>"),         // p23
        ("tokenize 32 X | var %w a$1 | out %w", "<a$1>"),    // p24
        ("tokenize 32 X | out $iif(1,a$1,b)", "<a$1>"),      // p25
        ("tokenize 32 X | out $!1 a$!1", "<$1 a$!1>"),       // p30
        ("out $chr(36) $+ 1", "<$1>"),                       // p31
        ("tokenize 32 X | out ~$1", "<~$1>"),                // p32
        ("tokenize 32 X | out $upper($1$1)", "<X$1>"),       // p43
        ("tokenize 32 X | out $replace(a$1,a,b)", "<b$1>"),  // p44
        ("out <$me> [$me] {$me}", "<<$me> [$me] {$me}>"),    // p48
        ("tokenize 32 X | out $1 $+ $1 a$+b", "<XX a$+b>"),  // p49
        ("set %v Q | out $+(%v,x%v)", "<Qx%v>"),             // p52
        ("tokenize 32 X | out ($1) ( $1 )", "<($1) ( X )>"), // p53
        ("tokenize 32 X | out $1 $+ ) $1)", "<X) X)>"),      // p55
        ("tokenize 32 X | out $!1x $!upper(a)x", "<$1x $upper(a)x>"), // v15
        // Inside a quoted run each word follows the rule (d3).
        (
            "tokenize 32 X | out \"/echo -a refresh $!1\" \"a $1 b\"",
            "<\"/echo -a refresh $1\" \"a X b\">",
        ),
    ]);
}

#[test]
fn a_word_start_name_takes_the_rest_of_the_word() {
    check(&[
        ("tokenize 32 X | out $1, $1. $1: $me: $me.", "<X, X. X:>"), // p18
        ("set %v Q | out %v, %v. %v:", "<>"),                        // p19
        ("set %v Q | out %v~ %v- %v)", "<>"),                        // p35
        ("set %v.a Z | out %v.a %v.a. x%v.a", "<Z x%v.a>"),          // p54
        ("set %a A | set %b B | out %a%b", "<>"),                    // p09
        ("set %v Q | out %v%v", "<>"),                               // p21
        ("out $me's", "<>"),                                         // w11
        ("set %v Q | out %v's", "<>"),                               // w13
        ("tokenize 32 X | out $1x $1.x $1_x $1:x", "<Xx X.x X_x X:x>"), // v02
        ("tokenize 32 X | out $1~ $1- $1-x", "<X~ X Xx>"),           // p33
        ("tokenize 32 X Y | out $1-2x $2-x", "<X Yx Yx>"),           // w14
        (
            "tokenize 32 X Y | out $gettok(a.b.c,$0,46) x$0 $0x",
            "<b x$0 2x>",
        ), // v20
        ("tokenize 32 X | out $1- x", "<X x>"),                      // w15
        (
            "set %v Q | out $upper(%v) $upper(%v.) $upper( %v ) $upper(%v%v)",
            "<Q Q>",
        ), // v06
        ("set %v Q | out [ %v ] [ %v. ]", "<Q>"),                    // v07
    ]);
}

#[test]
fn hash_dollar_adds_a_channel_prefix_to_parameters_only() {
    check(&[
        ("tokenize 32 X | out #$1", "<#X>"),  // p11
        ("tokenize 32 #X | out #$1", "<#X>"), // p12
        (
            "tokenize 32 X | out #$upper(ab) #$1- #$+(a,b) ##$1 #x$1",
            "<AB #X ab ##$1 #x$1>",
        ), // v03
        ("tokenize 32 X | out #$1, #$1x", "<#X, #Xx>"), // v04
        ("set %v Q | out #%v &%v", "<#%v &%v>"), // v05
        ("tokenize 32 | out a #$1 b", "<a b>"), // w01
        ("tokenize 32 &x | out #$1", "<&x>"), // w02
    ]);
}

#[test]
fn ircx_channel_names_stay_literal() {
    // jIRC extension (IRCX): mIRC itself would read `%#Room` as a variable.
    check(&[("out %#Room %&Lobby", "<%#Room %&Lobby>")]);
}

#[test]
fn conditions_evaluate_operands_inside_parentheses_and_negations() {
    check(&[
        (
            "tokenize 32 X | if (a$1 == aX) out yes | else out no",
            "<no>",
        ), // p07
        (
            "tokenize 32 X | if ($1 == X) out yes | else out no",
            "<yes>",
        ), // p08
        ("tokenize 32 X | out $iif(a$1 == aX,y,n)", "<n>"), // p39
        ("set %v Q | if (x%v == xQ) out yes | else out no", "<no>"), // p46
        (
            "tokenize 32 X | if (X isin a$1) out yes | else out no",
            "<no>",
        ), // p47
        ("set %a 5 | if (%a+1 == 6) out yes | else out no", "<no>"), // p50
        ("tokenize 32 X | if (!$1) out empty | else out set", "<set>"), // w07
        ("tokenize 32 | if (!$1) out empty | else out set", "<empty>"), // w08
        ("set %v Q | if (!%v) out empty | else out set", "<set>"), // w09
        ("tokenize 32 X | out $iif(!$1,e,s) $iif($1,s,e)", "<s s>"), // w10
        (
            "tokenize 32 X | if (($1 == X)) out yes | else out no",
            "<yes>",
        ), // e3
        ("set %v Q | if (Q == %v) out yes | else out no", "<yes>"), // e4
        (
            "set %v Q | if ((Q == %v) && (%v == Q)) out yes | else out no",
            "<yes>",
        ), // e5
        (
            "tokenize 32 X | if ($len($1) == 1) out yes | else out no",
            "<yes>",
        ), // e6
        (
            "tokenize 32 X | if (!$len($1)) out empty | else out set",
            "<set>",
        ), // e7
        ("set %v Q | out $iif((Q == %v),y,n) $iif(!%v,e,s)", "<y s>"), // e8
        (
            "tokenize 32 X | if ($1 != Y) out yes | else out no",
            "<yes>",
        ), // e9
        (
            "tokenize 32 X | if ($1 !isin abc) out yes | else out no",
            "<yes>",
        ), // e10
        ("set %v Q | if (%v) out yes | else out no", "<yes>"), // v08
        ("set %v Q | if ((%v == Q)) out yes | else out no", "<yes>"), // v09
        (
            "set %v Q | out $iif(%v == Q,y,n) $iif((%v == Q),y,n)",
            "<y y>",
        ), // v11
    ]);
}

#[test]
fn calc_arguments_also_evaluate_after_an_operator() {
    check(&[
        (
            "set %a 5 | out $calc(1+%a) $calc(%a+1) $calc(%a+%a)",
            "<6 0 0>",
        ), // p40
        ("tokenize 32 3 | out $calc(2*$1) $calc($1*2)", "<6 6>"), // p41
        (
            "set %a 5 | out $calc(1 + %a) $calc(%a) $calc(%a + 1)",
            "<6 5 6>",
        ), // v13
        (
            "tokenize 32 7 | out $calc($1+1) $calc(($1+1)*2) $calc(-$1)",
            "<8 16 -7>",
        ), // v17
        (
            "set %a 5 | out $calc(1+%a) $calc(1-%a) $calc(1/%a) $calc(1^%a) $calc(7 % %a)",
            "<6 -4 0.2 1 2>",
        ), // w06
        (
            "set %a 5 | tokenize 32 3 | out $calc(%a*$1) $calc($1*%a)",
            "<0 15>",
        ), // c2
        (
            "tokenize 32 3 | out $calc($len(ab)*$1) $calc($len(ab)*$len(abc))",
            "<6 6>",
        ), // e2, c1
        (
            "tokenize 32 X | out $calc(10-$len(abc)) $calc(2*$len(ab)+1)",
            "<7 5>",
        ), // v12
        ("set %a 5 | out $calc(2*%a) $calc(%a*2+1)", "<10 0>"),   // w04
        ("tokenize 32 3 | out $calc(2*$1+1)", "<7>"),             // w05
    ]);
}

#[test]
fn dollar_star_repeats_the_command_once_per_token() {
    let aliases = "alias zz { out in $* }\nalias zz2 { out zz2 $1- }";
    for (body, expected) in [
        (
            "tokenize 32 a b c | out x $* y",
            vec!["<x a y>", "<x b y>", "<x c y>"],
        ), // z01
        ("tokenize 44 a,b | out $*", vec!["<a>", "<b>"]), // z02
        ("tokenize 32 a b | out $* $*", vec!["<a a>", "<b b>"]), // z03
        ("tokenize 32 a b | out $* $1 $0", vec!["<a a 2>", "<b a 2>"]), // z04
        ("tokenize 32 a b | out $upper($*)", vec!["<a>", "<b>"]), // z05
        ("tokenize 32 abcd | out $mid($*,2)", vec!["<~$*>"]), // z06
        (
            "tokenize 32 abcd | out $left($*,1) $+ - $+ $mid($*,2)",
            vec!["<`-~$*>"],
        ), // z07
        ("tokenize 32 | out x $*", vec![]),               // z08
        (
            "tokenize 32 a b | out $* | out after",
            vec!["<a>", "<b>", "<after>"],
        ), // z09
        (
            "tokenize 32 1 2 3 | out $* | tokenize 32 4 5 6 7 8 | out $* | out here",
            vec!["<1>", "<2>", "<3>", "<7>", "<8>", "<here>"],
        ), // z10
        (
            "tokenize 32 1 2 | out $* | tokenize 32 3 4 | out $* | out here",
            vec!["<1>", "<2>", "<here>"],
        ), // z11
        (
            "tokenize 32 a b | out $* | zz c d",
            vec!["<a>", "<b>", "<in c>", "<in d>"],
        ), // z12
        ("tokenize 32 a b | out x$*", vec!["<x$*>"]),     // z13
        ("tokenize 32 a b | out $* $+ !", vec!["<a>", "<b>"]), // z14
        ("tokenize 32 $!time b | out $*", vec!["<$time>", "<b>"]), // z15
        (
            "set %c 0 | tokenize 32 1 2 3 | inc %c $* | out %c",
            vec!["<6>"],
        ), // z16
        ("tokenize 32 a b | out [ $* ]", vec!["<a>", "<b>"]), // z20
        (
            "tokenize 32 a b | out $+(<,$*,>)",
            vec!["<<`~$*>>", "<<`~$*>>"],
        ), // z21
        ("tokenize 32 a b | out $len($*) $*", vec!["<4 a>", "<4 b>"]), // z22
        ("tokenize 32 a b | out $*x", vec!["<a>", "<b>"]), // z23
        ("tokenize 32 a b | zz2 $*", vec!["<zz2 a>", "<zz2 b>"]), // z24
        ("tokenize 32 a b | var %t $* | out %t", vec!["<b>"]), // z25
        ("tokenize 32 a b | set %t2 $* | out %t2", vec!["<b>"]), // z26
        (
            "tokenize 32 a $!upper(x) | out $*",
            vec!["<a>", "<$upper(x)>"],
        ), // z27
        (
            "tokenize 32 a b | out $1 | out $* | out $1-",
            vec!["<a>", "<a>", "<b>", "<a b>"],
        ), // z28
    ] {
        assert_eq!(run(aliases, body), expected, "{body}");
    }
    // A built-in command repeats the same way (the KB's own example).
    assert_eq!(run("", "tokenize 32 a b | echo -a $*"), ["a", "b"]);
}

// Batch 2 review: native mIRC 7.79 pins from the independent verifier
// (analysis/rea-batch2-20261109/verifier-n1.mrc, verifier-n2.mrc).

#[test]
fn a_closing_parenthesis_ends_a_name_inside_calc() {
    check(&[
        ("set %a 5 | set %b 3 | out $calc((%a + %b) / 2)", "<4>"), // n01
        ("set %t 1000 | out $calc((5000 - %t) / 1000)", "<4>"),    // n02
        (
            "set %a 5 | out $calc((%a)) $calc(1+(%a)) $calc((%a)*2) $calc((%a)^2)",
            "<5 6 10 25>",
        ), // n03
        (
            "out $calc(($pi)) $calc(2*($pi)) $calc(($pi)+1)",
            "<3.141593 6.283185 4.141593>",
        ), // n04
        ("set %a 5 | out $calc(%a / (2 * %a))", "<0.5>"),          // n05
        ("set %a 1 | out $calc(%a + (%a))", "<2>"),                // n06
        ("set %a.b 5 | out $calc((%a.b))", "<5>"),                 // n07
        ("set %a 5 | out $calc( (%a ) ) $calc(-(%a))", "<5 -5>"),  // n09
        ("out $calc(($pi - $pi) / 1000)", "<0>"), // n10 shape (`$ticks` can tick between calls)
        ("set %a 5 | out $calc((%a))) $calc(((%a)))", "<5 5>"), // n11
        ("set %v Q | out $upper((%v)) $upper(%v))", "<(%V) Q>"), // n12
    ]);
}

#[test]
fn an_empty_parameter_takes_the_text_glued_to_it_along() {
    check(&[
        ("tokenize 32 X | out a $5test b $1test", "<a b Xtest>"), // n20
        ("tokenize 32 X | out a $2- b $2-x c", "<a b c>"),        // n21
        ("tokenize 32 X | out a #$2x b", "<a b>"),                // n22
        (
            // k0207, the KB's own `$1-` example
            "tokenize 32 Khaled xyz parm3 | out $1foo2bar vs $1 $+ foo123bar and $1$2 vs $1 $+ $2 and $3test and $1-x2 but $5test is null",
            "<Khaledfoo2bar vs Khaledfoo123bar and Khaled$2 vs Khaledxyz and parm3test and Khaled xyz parm3x2 but is null>",
        ),
    ]);
}

#[test]
fn quotes_are_ordinary_text() {
    check(&[
        ("tokenize 32 X | out \" $+ $1 $+ \"", "<\"X\">"), // n40
        ("tokenize 32 X | out a \" $+ $1 $+ \" b", "<a \"X\" b>"), // n41
        ("tokenize 32 X | out \"a $+ $1\"", "<\"aX\">"),   // n42
    ]);
}

#[test]
fn regsubex_markers_are_tokens_of_their_own() {
    check(&[
        ("out $regsubex(abc,/(b)/,\\1$chr(45))", "<ab-c>"), // r01
        ("out $regsubex(abc,/(b)/,$chr(45)\\1)", "<a-bc>"), // r02
        ("out $regsubex(abc,/(b)/,$upper(\\1)x)", "<aBc>"), // r04
        (
            "set %s Q | out $regsubex(abc,/(b)/,\\1%s) $regsubex(abc,/(b)/,%s\\1)",
            "<abQc aQbc>",
        ), // r05
    ]);
    // A capture is data: it is never evaluated, whatever it contains.
    check(&[("out $regsubex($!me x,/(\\S+)/,[\\1])", "<[$me] x>")]);
}

#[test]
fn dollar_star_scope_follows_aliases_handlers_and_routed_commands() {
    let handler = "on *:SIGNAL:vs:{ out sig $* }";
    for (body, expected) in [
        (
            "tokenize 32 a b c | out $* | signal -n vs x y",
            vec!["<a>", "<b>", "<c>", "<sig x>", "<sig y>"],
        ), // g01
        (
            "tokenize 32 a b c | signal -n vs x y | out $*",
            vec!["<sig x>", "<sig y>", "<a>", "<b>", "<c>"],
        ), // g02
        (
            "tokenize 32 a b c | signal -n vs x y z w | out $*",
            vec![
                "<sig x>", "<sig y>", "<sig z>", "<sig w>", "<a>", "<b>", "<c>",
            ],
        ), // g03
        (
            "tokenize 32 a b c | var %i 1 | while (%i <= 2) { out %i $* | inc %i }",
            vec!["<1 a>", "<1 b>", "<1 c>"],
        ), // n30
        (
            "tokenize 32 a b c | out $* $$2 | out after",
            vec!["<a b>", "<b b>", "<c b>", "<after>"],
        ), // n32
        (
            "tokenize 32 a b c | if (1) out $*",
            vec!["<a>", "<b>", "<c>"],
        ), // n33
        (
            "tokenize 32 a b c | out $* | goto x | out no | :x | out $* end",
            vec!["<a>", "<b>", "<c>"],
        ), // n37
        (
            "tokenize 32 a b c | scon -r out $*",
            vec!["<a>", "<b>", "<c>"],
        ), // n31
    ] {
        assert_eq!(run(handler, body), expected, "{body}");
    }
}

#[test]
fn regsubex_markers_work_inside_grouping_parentheses() {
    // KB `$hotp` example k0337: `\n` inside `$calc(97+ (\n % 6))`.
    check(&[
        (
            "out $regsubex(xxx,/x/g,$calc(97+ (\\n % 6)) $+ -)",
            "<98-99-100->",
        ),
        ("out $regsubex(ab,/(.)/g,$calc((\\n)*10))", "<1020>"),
    ]);
}

// Batch 2 review round 2: native mIRC 7.79 pins from
// analysis/rea-batch2-20261109/ (native-vr2-*.txt from the verifier,
// native-b2r2-q.txt and native-b2r2-s.txt).

#[test]
fn calc_reads_variables_and_identifiers_anywhere_in_a_word() {
    check(&[
        (
            "out $calc(10%3) $calc(7%2) $calc(2^3%2) $calc((7)%2)",
            "<10 7 8 7>",
        ), // k28, k06, q23
        ("set %a 5 | out $calc((%a)%2) $calc(-(%a)%2)", "<5 -5>"), // q08
        (
            "set %a 5 | set %c *2 | out $calc(3%c) $calc((%a)%c) $calc(3.%c)",
            "<6 10 6>",
        ), // k01, k16, k09
        (
            "tokenize 32 *2 3 | out $calc(3$1) $calc((5)$1) $calc(($2)$1)",
            "<6 10 6>",
        ), // k10, k13, k23
        // After `$N`, an operator still lets a variable in, not an identifier.
        ("set %a 5 | tokenize 32 2 3 | out $calc($1*%a)", "<10>"), // b2r2-q14
        ("set %a 5 | out $calc(1+2%a*3)", "<3>"),                  // b2r2-q16
    ]);
}

#[test]
fn dollar_star_line_is_evaluated_once_and_repeats_only_when_star_is_evaluated() {
    let cnt = "alias cnt { inc %cnt | return %cnt }";
    for (body, expected) in [
        (
            "tokenize 32 a b c | set %cnt 0 | out $* $cnt",
            vec!["<a 1>", "<b 1>", "<c 1>"],
        ), // verifier finding 4
        (
            "tokenize 32 a b | var %acc = %acc $+ $* | out %acc",
            vec!["<b>"],
        ),
        (
            "tokenize 32 a b | out ($*) | out $*",
            vec!["<($*)>", "<a>", "<b>"],
        ), // s12
        ("tokenize 32 a b | out a,$*", vec!["<a,$*>"]),
        ("tokenize 32 a b | out $eval($*,0)", vec!["<$*>"]),
        ("tokenize 32 a b | out #$* x", vec!["<a x>", "<b x>"]), // s08
        ("tokenize 32 a b | out $$* x", vec!["<a x>", "<b x>"]), // s05
        ("tokenize 32 a b | out [ $* ]", vec!["<a>", "<b>"]),    // c40
        (
            "tokenize 32 a b | out $mid($*,2) | out $*",
            vec!["<~$*>", "<~$*>"],
        ), // s01
        (
            "tokenize 32 a b | out $len($*) | out $*",
            vec!["<4>", "<4>"],
        ), // s19
        (
            "tokenize 32 a b | out $iif($*,y,n) | out $*",
            vec!["<y>", "<a>", "<b>"],
        ), // s07
        ("tokenize 32 a b | out $iif($* == a,yes,no)", vec!["<no>"]), // z18
        ("tokenize 32 a b | out $iif(1,$*,n)", vec!["<a>", "<b>"]), // s17
        ("tokenize 32 a b | noop $* | out $*", vec![]),          // s13
        ("tokenize 32 | out $* x | out after", vec!["<after>"]), // s03
        ("tokenize 32 | out $$* x | out after", vec!["<after>"]), // s04
    ] {
        assert_eq!(run(cnt, body), expected, "{body}");
    }
}

#[test]
fn regsubex_capture_in_literal_text_is_plain_text() {
    // mIRC prints the raw `$1` there (b31, r06); jIRC prints the capture.
    // Either way no internal marker may reach the output.
    check(&[
        (
            "out $regsubex(abc,/(b)/,$iif(1,x$lower(\\1)))",
            "<ax$lower(b)c>",
        ),
        (
            "out $regsubex(abc,/(b)/,$upper(x$lower(\\1)))",
            "<aX$LOWER(B)c>",
        ),
        ("out $regsubex(abc,/(b)/,[\\1])", "<a[b]c>"),
    ]);
}

// Batch 2 review round 3 (analysis/rea-batch2-20261109/native-vr3-*.txt).

#[test]
fn regsubex_capture_never_runs_as_script() {
    // A capture holding parentheses and identifiers stays text, even inside
    // literal text where it can't use the normal sealed-word path (a26, h10,
    // i28). mIRC prints `$1` there; jIRC prints the capture as text.
    let pwn = "alias pwn { out PWNED | return x }";
    for (body, expected) in [
        (
            "set %p ) $!pwn $!upper(zz) ( | out $regsubex(%p,/(.+)/,x(\\1))",
            "<x() $pwn $upper(zz) ()>",
        ),
        (
            "set %p ) $!pwn ( | out $regsubex(%p,/(.+)/,[$upper(\\1)])",
            "<[$upper() $pwn ()]>",
        ),
        (
            "set %p ) $!pwn ( | out $regsubex(%p,/(.+)/,<$lower(\\1)>)",
            "<<$lower() $pwn ()>>",
        ),
        (
            "out $regsubex($chr(40),/(.+)/,x(\\1) $upper(y))",
            "<x(() Y>",
        ),
    ] {
        assert_eq!(run(pwn, body), [expected], "{body}");
    }
}

#[test]
fn regsubex_marker_inside_the_eval_short_form() {
    check(&[("out $regsubex(abc,/(b)/,$(\\1))", "<abc>")]); // vr3 d13
}

#[test]
fn dollar_star_reads_the_remaining_tokens_afresh() {
    check(&[("tokenize 32 a b c | tokenize 32 $* | out $1-", "<a>")]); // vr3
}

// Batch 2 review round 4 (analysis/rea-batch2-20261109/native-vr4-a.txt).

#[test]
fn hfind_callback_is_evaluated_once() {
    let pwn = "alias pwn { out PWNED | return x }";
    for (body, expected) in [
        (
            "hadd -m h17 $!pwn 1 | var %n = $hfind(h17,*,0,w,out $1-) | out n= %n",
            vec!["<$pwn>", "<n= 1>"],
        ), // n17
        (
            "hadd -m h18 $!pwn 1 | var %n = $hfind(h18,*,0,w,out $!1-) | out n= %n",
            vec!["<$1->", "<n= 1>"],
        ), // n18
        (
            "hadd -m h23 $+(%,secret) 1 | var %n = $hfind(h23,*,0,w,out $1-) | out n= %n",
            vec!["<%secret>", "<n= 1>"],
        ), // n23
        (
            "hadd -m h25 $!pwn 1 | noop $hfind(h25,*,0,w,out <$1->)",
            vec!["<<$1->>"],
        ), // n25
        (
            "hadd -m h26 $!pwn 1 | noop $hfind(h26,*,0,w,out $upper($1-))",
            vec!["<$PWN>"],
        ), // n26
        // A `$regsubex` capture inside the callback stays text (mIRC reads
        // `\1` there as the callback's own `$1`, n20).
        (
            "hadd -m h20 a 1 | var %t = ) $!pwn ( | noop $regsubex(%t,/(.+)/,$hfind(h20,*,0,w,out <\\1>))",
            vec!["<<) $pwn (>>"],
        ),
    ] {
        assert_eq!(run(pwn, body), expected, "{body}");
    }
}
