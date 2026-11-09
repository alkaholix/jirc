use super::{eval::Action, RunCtx, ScriptEngine};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

fn fixture() -> RunCtx<'static> {
    let data_dir = std::env::temp_dir().join(format!(
        "jirc-file-read-{}-{}",
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
fn file_reads_binary_form_replaces_the_destination_and_retains_raw_bytes() {
    let ctx = fixture();
    let file = ctx.data_dir.join("payload.bin");
    std::fs::write(&file, [0, 255, 65, 254, 67]).unwrap();
    let engine = ScriptEngine::new();
    engine.load(
        "alias check {\n\
          fopen h payload.bin\n\
          bset &out 1 1 2 3 4 5 6\n\
          var %n = $fread(h,3,&out)\n\
          echo -s first= $+ %n length= $+ $bvar(&out,0) bytes= $+ $bvar(&out,1,3) position= $+ $fopen(h).pos eof= $+ $feof error= $+ $ferr\n\
          var %n = $fread(1,4,&out)\n\
          echo -s partial= $+ %n length= $+ $bvar(&out,0) bytes= $+ $bvar(&out,1,2) position= $+ $fopen(h).pos eof= $+ $feof error= $+ $ferr\n\
          var %n = $fread(1,1,&out)\n\
          echo -s end= $+ %n length= $+ $bvar(&out,0) position= $+ $fopen(h).pos eof= $+ $feof error= $+ $ferr\n\
          fclose h\n\
        }",
    );
    assert_eq!(
        echoes(engine.run_alias(&ctx, "(status)", "check", "")),
        [
            "first=3 length=3 bytes=0 255 65 position=3 eof=$false error=$false",
            "partial=2 length=2 bytes=254 67 position=5 eof=$false error=$false",
            "end=0 length=0 position=5 eof=$true error=$false"
        ]
    );
    assert_eq!(std::fs::read(&file).unwrap(), [0, 255, 65, 254, 67]);
    std::fs::remove_file(file).unwrap();
    std::fs::remove_dir(ctx.data_dir).unwrap();
}

#[test]
fn file_reads_numeric_text_and_character_selectors_match_fopen_enumeration() {
    let ctx = fixture();
    let first = ctx.data_dir.join("first.txt");
    let second = ctx.data_dir.join("second.txt");
    std::fs::write(&first, b"alpha\r\nremaining").unwrap();
    std::fs::write(&second, b"Zed").unwrap();
    let engine = ScriptEngine::new();
    engine.load(
        "alias check {\n\
          fopen z second.txt\n\
          fopen a first.txt\n\
          echo -s names= $+ $fopen(1) $+ / $+ $fopen(2) counts= $+ $fread(0) $+ / $+ $fgetc(0)\n\
          echo -s line= $+ $fread(1) character= $+ $fgetc(2)\n\
          echo -s remaining= $+ $freadex(1) positions= $+ $fopen(a).pos $+ / $+ $fopen(z).pos\n\
          fclose *\n\
        }",
    );
    assert_eq!(
        echoes(engine.run_alias(&ctx, "(status)", "check", "")),
        [
            "names=a/z counts=2/2",
            "line=alpha character=Z",
            "remaining=remaining positions=16/1"
        ]
    );
    std::fs::remove_file(first).unwrap();
    std::fs::remove_file(second).unwrap();
    std::fs::remove_dir(ctx.data_dir).unwrap();
}

#[test]
fn file_reads_invalid_binary_arguments_preserve_the_pointer_and_destination() {
    let ctx = fixture();
    let file = ctx.data_dir.join("payload.bin");
    std::fs::write(&file, b"ABCDEF").unwrap();
    let engine = ScriptEngine::new();
    engine.load(
        "alias check {\n\
          fopen h payload.bin\n\
          bset &out 1 88 89\n\
          noop $fread(h,0,&out)\n\
          echo -s zero= $+ $ferr position= $+ $fopen(h).pos bytes= $+ $bvar(&out,1,2)\n\
          noop $fread(h,2,wrong)\n\
          echo -s target= $+ $ferr position= $+ $fopen(h).pos bytes= $+ $bvar(&out,1,2)\n\
          noop $fread(h,33554433,&out)\n\
          echo -s limit= $+ $ferr position= $+ $fopen(h).pos bytes= $+ $bvar(&out,1,2)\n\
          noop $fread(missing,2,&out)\n\
          echo -s missing= $+ $ferr position= $+ $fopen(h).pos bytes= $+ $bvar(&out,1,2)\n\
          noop $fread(h,2,&out)\n\
          echo -s recovered= $+ $ferr position= $+ $fopen(h).pos bytes= $+ $bvar(&out,1,2)\n\
          fclose h\n\
        }",
    );
    assert_eq!(
        echoes(engine.run_alias(&ctx, "(status)", "check", "")),
        [
            "zero=$true position=0 bytes=88 89",
            "target=$true position=0 bytes=88 89",
            "limit=$true position=0 bytes=88 89",
            "missing=$true position=0 bytes=88 89",
            "recovered=$false position=2 bytes=65 66"
        ]
    );
    std::fs::remove_file(file).unwrap();
    std::fs::remove_dir(ctx.data_dir).unwrap();
}
