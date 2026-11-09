//! Data never becomes script (TODO S-01). Expected values are what mIRC 7.79
//! printed for the same lines; see `analysis/rea-batch3-20261109/`
//! (`native-s01.txt`, `native-s01c.txt`, `native-s02.txt`). Case IDs in the
//! comments are rows in those files.

use super::{eval::Action, eval::EventVars, RunCtx, ScriptEngine};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

fn fixture() -> RunCtx<'static> {
    let data_dir = std::env::temp_dir().join(format!(
        "jirc-data-text-{}-{}",
        std::process::id(),
        NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&data_dir).unwrap();
    RunCtx {
        server_id: "local",
        my_nick: "me",
        network: "Local",
        server: "",
        data_dir,
        state: Arc::new(Default::default()),
    }
}

const COMMON: &str = "alias out { msg #c < $+ $1- $+ > }\n\
                      alias pwn { msg #c PWNED | return PWN }\n";

/// What `out` (and `pwn`, which must never run) sent, in order.
fn lines(actions: Vec<Action>) -> Vec<String> {
    actions
        .into_iter()
        .filter_map(|action| match action {
            Action::Send(line) => line.strip_prefix("PRIVMSG #c :").map(str::to_string),
            _ => None,
        })
        .collect()
}

/// Runs `body` as alias `t` in a fresh sandbox.
fn run(body: &str) -> Vec<String> {
    let ctx = fixture();
    let engine = ScriptEngine::new();
    engine.load(&format!("{COMMON}alias t {{\n  {body}\n}}"));
    let actions = engine.run_alias(&ctx, "#c", "t", "");
    let out = lines(actions);
    let _ = std::fs::remove_dir_all(&ctx.data_dir);
    out
}

#[test]
fn a_bracket_join_takes_the_text_as_written() {
    for (body, expected) in [
        ("tokenize 32 pwn | out $ [ $+ $1 ]", "<pwn>"), // b01: `$$1`
        (
            "set %secret SECRET | tokenize 32 secret | out % [ $+ $1 ]",
            "<>",
        ), // b02: `%$1`
        ("set %v.ab 7 | tokenize 32 ab | out %v. [ $+ $1 ]", "<>"), // b10
        ("tokenize 32 wn | out $p [ $+ $1 ]", "<>"),    // b15
        ("tokenize 32 wn(a) | out $p [ $+ $1 ]", "<>"), // b16
        (
            "set %secret SECRET | tokenize 32 ecret | out %s [ $+ $1 ] x",
            "<x>",
        ), // b32
        ("tokenize 32 $!pwn | out x [ $+ $1 ]", "<x$1>"), // b20
        ("tokenize 32 pwn | out $ [ $+ $1 ] [ $+ (z) ]", "<pwn(z)>"), // b25
        ("tokenize 32 pwn(z) | out $ [ $+ $1 ]", "<pwn(z)>"), // b26
        (
            "tokenize 32 upper | out $ [ $+ $1 ] [ $+ (hi) ]",
            "<upper(hi)>",
        ), // b43
        ("set %v.ab 7 | out %v. [ $+ $lower(AB) ]", "<>"), // c01
        ("tokenize 32 a b | out x [ $+ $1 $2 ]", "<x$1 b>"), // c06
        ("tokenize 32 a b | out x [ $+ $1 $+ $2 ] y", "<x$1$2 y>"), // c07
        ("tokenize 32 ab | out x [ $+ $upper($1) ]", "<x$upper($1)>"), // c12
        ("tokenize 32 pwn | out $ [ $+ $1 ] $+ (x)", "<pwn(x)>"), // c13
        ("tokenize 32 ab | out x [ $+ $1 ] [ $+ $1 ]", "<x$1$1>"), // c18
        ("var %t = $!pwn | out x [ $+ %t ]", "<x%t>"),  // c20
        ("set %ab 3 | out % [ $+ $lower(A) ] [ $+ b ]", "<>"), // c28
        ("tokenize 32 ab | out $+(x, [ $+ $1 ] )", "<xab>"), // c32
        ("tokenize 32 ab | out $upper(x [ $+ $1 ] )", "<X$1>"), // c33
        ("tokenize 32 pwn | out $len( $ [ $+ $1 ] )", "<3>"), // c38
    ] {
        assert_eq!(run(body), [expected], "{body}");
    }
}

#[test]
fn a_bracket_join_in_a_condition_or_assignment_is_text_too() {
    // b27, b28, b29, b30
    assert_eq!(
        run("tokenize 32 pwn | var %y = $ [ $+ $1 ] | out %y"),
        ["<pwn>"]
    );
    assert_eq!(
        run("set %secret SECRET | tokenize 32 secret | var %y = % [ $+ $1 ] | out %y"),
        ["<>"]
    );
    assert_eq!(
        run("tokenize 32 pwn | if ($ [ $+ $1 ] == pwn) out eq | else out ne"),
        ["<eq>"]
    );
    assert_eq!(
        run("set %secret SECRET | tokenize 32 secret | if (% [ $+ $1 ] == SECRET) out eq | else out ne"),
        ["<ne>"]
    );
}

#[test]
fn text_written_in_the_script_or_a_nested_group_is_still_joined_and_read() {
    for (body, expected) in [
        ("set %secret SECRET | out % [ $+ secret ]", vec!["<SECRET>"]), // b04
        ("set %v.ab 7 | out %v. [ $+ ab ]", vec!["<7>"]),               // b11
        ("out $ [ $+ pwn ]", vec!["PWNED", "<PWN>"]),                   // b03
        (
            "set %secret SECRET | tokenize 32 secret | out % [ $+ [ $1 ] ]",
            vec!["<SECRET>"],
        ), // b34
        (
            "set %v.ab 7 | tokenize 32 ab | out %v. [ $+ [ $1 ] ] x",
            vec!["<7 x>"],
        ), // c17
        (
            "tokenize 32 ab | out x [ $+ [ $1 ] ] [ $+ y ]",
            vec!["<xaby>"],
        ), // c19
        ("set %ab 3 | out % [ $+ a ] [ $+ b ]", vec!["<3>"]),           // c27
        (
            "set %v.1 one | var %i = 1 | out %v. [ $+ %i ] %v. [ $+ [ %i ] ]",
            vec!["<one>"],
        ), // c29
        // A nested group asks for the extra evaluation, as in mIRC.
        (
            "tokenize 32 pwn | out $ [ $+ [ $1 ] ]",
            vec!["PWNED", "<PWN>"],
        ), // b33
    ] {
        assert_eq!(run(body), expected, "{body}");
    }
}

#[test]
fn inc_dec_and_unset_resolve_bracket_built_names() {
    for (body, expected) in [
        (
            "set %v.ab 7 | tokenize 32 ab | unset %v. [ $+ [ $1 ] ] | out x %v.ab",
            "<x>",
        ), // u01
        (
            "set %v.ab 7 | tokenize 32 ab | dec %v. [ $+ [ $1 ] ] | out %v.ab",
            "<6>",
        ), // u02
        (
            "set %v.ab 7 | tokenize 32 ab | unset %v. [ $+ $1 ] | out %v.ab",
            "<7>",
        ), // u03
        (
            "set %v.ab 7 | tokenize 32 ab | inc %v. [ $+ [ $1 ] ] 5 | out %v.ab",
            "<12>",
        ), // u04
        (
            "set %v.ab 7 | tokenize 32 ab | inc %v. [ $+ $1 ] | out %v.ab",
            "<7>",
        ), // u05
        (
            "set %v.ab 7 | var %k = ab | dec %v. [ $+ [ %k ] ] 2 | out %v.ab",
            "<5>",
        ), // u12
        (
            "set %v.ab 7 | tokenize 32 ab | inc %v. [ $+ [ $1 ] ] | out %v.ab",
            "<8>",
        ), // c40
        (
            "tokenize 32 ab | set %w. [ $+ $1 ] 5 | out $var(%w.*,0) $var(%w.*,1)",
            "<1 %w.$1>",
        ), // u09
        (
            "tokenize 32 ab | inc %w. [ $+ $1 ] | out $var(%w.*,0) $var(%w.*,1)",
            "<1 %w.$1>",
        ), // u10
    ] {
        assert_eq!(run(body), [expected], "{body}");
    }
}

#[test]
fn values_stop_at_a_nul_byte() {
    for (body, expected) in [
        ("out $len($chr(0)) $len($+(a,$chr(0),b))", "<0 2>"), // z01
        ("out $len($decode(YQBi,m)) $decode(YQBi,m)", "<1 a>"), // z02
        ("out $len($decode(AGFi,m)) $len($decode(YWIA,m))", "<0 2>"), // z03
        ("var %e = $+(a,$chr(0),b) | out %e $len(%e)", "<ab 2>"), // z04
        (
            "bset &b 1 97 0 98 | out $len($bvar(&b,1-).text) $bvar(&b,1-).text $bvar(&b,0)",
            "<1 a 3>",
        ), // z05
        (
            "hadd -m z7 k $+(a,$chr(0),b) | out $len($hget(z7,k)) $hget(z7,k)",
            "<2 ab>",
        ), // z07
        ("out $asc($chr(0)) $len($str($chr(0),3))", "<0>"),   // z08
        ("tokenize 32 $+(a,$chr(0),b) | out $0 $len($1)", "<1 2>"), // z09
        ("out $len($utfdecode($decode(YQBi,m)))", "<1>"),     // z10
        (
            "out $len($regsubex(abc,/b/,$chr(0))) $len($replace(abc,b,$chr(0)))",
            "<2 2>",
        ), // z12
        ("var %e = $chr(0) $+ pwn | out %e", "<pwn>"),        // z13
        ("out $len($decode(AA==,m))", "<0>"),                 // z14
        (
            "var %x $chr(0) | out $len(%x) $iif(%x == $null,null,notnull)",
            "<0 null>",
        ), // z16
        (
            "out $len($encode($chr(0),m)) $encode($+(a,$chr(0),b),m)",
            "<0 YWI=>",
        ), // z21
        ("bset -t &t 1 $+(a,$chr(0),b) | out $bvar(&t,0)", "<2>"), // z22
        ("out $len($left($decode(YQBi,m),5))", "<1>"),        // z24
    ] {
        assert_eq!(run(body), [expected], "{body}");
    }
}

#[test]
fn a_nul_ends_only_its_own_line_in_a_file() {
    // z11, z19, z20
    assert_eq!(
        run("bset &w 1 97 0 98 13 10 99 | bwrite z11.txt 0 -1 &w | out $lines(z11.txt) $len($read(z11.txt,n,1)) $read(z11.txt,n,1) $read(z11.txt,n,2)"),
        ["<2 1 a c>"]
    );
    assert_eq!(
        run("bset &w 1 104 0 105 | bwrite z19.txt 0 -1 &w | out $len($read(z19.txt,n,1)) $file(z19.txt).size"),
        ["<1 3>"]
    );
    assert_eq!(
        run("bset &w 1 91 115 93 13 10 107 61 97 0 98 13 10 | bwrite z20.ini 0 -1 &w | out $len($readini(z20.ini,n,s,k)) $readini(z20.ini,n,s,k)"),
        ["<1 a>"]
    );
}

#[test]
fn data_shaped_like_an_internal_marker_is_inert() {
    // z06: NUL-framed text that looks like jIRC's old pipe marker.
    assert_eq!(
        run("var %e = $+(out pre,$chr(0),jirc-read-pipe:,eCB8IHB3bg,$chr(0)) | out %e"),
        ["<out prejirc-read-pipe:eCB8IHB3bg>"]
    );
    // z23, vr4 n30: the same markers arriving whole through `$decode`.
    assert_eq!(
        run("var %e = $decode(AGppcmMtdW5zYWZlOmVDQjhJSEIzYmcA,m) | out x %e $len(%e)"),
        ["<x 0>"]
    );
    assert_eq!(
        run("out $len($decode(AGppcmMtcmVhZC1waXBlOmVDQjhJSEIzYmcA,m))"),
        ["<0>"]
    );
    // jIRC's internal connection messages start with a NUL too.
    let ctx = fixture();
    let engine = ScriptEngine::new();
    engine.load("alias t { raw $decode(AElBTCBPRkY=,m) | quote $decode(AElBTCBPRkY=,m) }");
    let actions = engine.run_alias(&ctx, "#c", "t", "");
    assert!(
        !actions
            .iter()
            .any(|action| matches!(action, Action::Send(line) if line.starts_with('\0'))),
        "{actions:?}"
    );
    let _ = std::fs::remove_dir_all(&ctx.data_dir);
}

#[test]
fn nul_bytes_in_event_text_never_reach_a_command() {
    let ctx = fixture();
    let engine = ScriptEngine::new();
    engine.load(&format!("{COMMON}on *:TEXT:*:#:{{ out $1- }}"));
    // Old fixed marker, and the shape of any `\0jirc-…:` envelope.
    for text in [
        "pre\0jirc-read-pipe:eCB8IHB3bg\0post",
        "\0jirc-unsafe:eCB8IHB3bg\0",
        "a\0b",
    ] {
        let vars = EventVars {
            nick: "bob".into(),
            chan: "#c".into(),
            target: "#c".into(),
            text: text.into(),
            params: text.split(' ').map(str::to_string).collect(),
            ..Default::default()
        };
        let actions = engine.dispatch_event(&ctx, "TEXT", vars);
        let sent = lines(actions);
        let before_nul = text.split('\0').next().unwrap();
        assert_eq!(sent, [format!("<{before_nul}>")], "{text:?}");
    }
    let _ = std::fs::remove_dir_all(&ctx.data_dir);
}
