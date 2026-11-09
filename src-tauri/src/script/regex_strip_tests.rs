use super::{eval::Action, RunCtx, ScriptEngine};

fn run(source: &str) -> Vec<String> {
    let context = RunCtx {
        server_id: "local",
        my_nick: "me",
        network: "Local",
        server: "",
        data_dir: std::env::temp_dir(),
        state: Default::default(),
    };
    let engine = ScriptEngine::new();
    engine.load(source);
    engine
        .run_alias(&context, "(status)", "check", "")
        .into_iter()
        .filter_map(|action| match action {
            Action::Echo { text, .. } => Some(text),
            _ => None,
        })
        .collect()
}

#[test]
fn regsubex_strip_no_match_preserves_original_native_formatting() {
    assert_eq!(
        run(r"alias check {
            var %source = $chr(3) $+ 04test
            echo -s $regsubex(%source,/(z)/gS,x)
            echo -s $regsubex(named,%source,/(z)/gS,x)
            echo -s $regsubex(%source,/(z)/g,x)
            var %source = $chr(2) $+ $chr(31) $+ word $+ $chr(15)
            echo -s $regsubex(%source,/missing/S,x)
        }"),
        ["\x0304test", "\x0304test", "\x0304test", "\x02\x1fword\x0f"]
    );
}

#[test]
fn strip_no_match_output_forms_keep_original_and_zero_count() {
    assert_eq!(
        run(r"alias check {
            var %source = $chr(3) $+ 04test
            var %n = $regsubex(named,%source,/(z)/gS,x,%dest)
            echo -s %n $len(%dest) $asc($left(%dest,1))
            var %n = $regsub(named,%source,/(z)/gS,x,%dest)
            echo -s %n $len(%dest) $asc($left(%dest,1))
            var %n = $regsubex(named,%source,/(z)/gS,x,&dest)
            echo -s %n $bvar(&dest,1,7)
            var %n = $regsub(named,%source,/(z)/gS,x,&dest)
            echo -s %n $bvar(&dest,1,7)
        }"),
        [
            "0 7 3",
            "0 7 3",
            "0 3 48 52 116 101 115 116",
            "0 3 48 52 116 101 115 116"
        ]
    );
}

#[test]
fn strip_match_still_uses_stripped_text_and_can_replace_with_empty() {
    assert_eq!(
        run(r"alias check {
            var %source = $chr(3) $+ 04test
            echo -s result= $+ [ $+ $regsubex(%source,/(test)/S,x) $+ ]
            echo -s result= $+ [ $+ $regsubex(%source,/(test)/S,) $+ ]
            var %n = $regsub(named,%source,/(test)/S,,%dest)
            echo -s %n length= $+ $len(%dest)
        }"),
        ["result=[x]", "result=[]", "1 length=0"]
    );
}

#[test]
fn strip_no_match_clears_captures_without_running_subtext() {
    assert_eq!(
        run(r"alias replacement {
            inc %replacements
            return x
        }
        alias check {
            set %replacements 0
            var %source = $chr(3) $+ 04test
            noop $regex(named,abc,/(b)/)
            noop $regsubex(named,%source,/(z)/gS,$replacement)
            echo -s calls= $+ %replacements captures= $+ $regml(named,0)
        }"),
        ["calls=0 captures=0"]
    );
}
