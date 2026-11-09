use super::{eval::Action, RunCtx, ScriptEngine};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

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
fn fopen_exclusive_switch_persists_between_runs_and_excludes_other_engine_writes() {
    let data_dir = std::env::temp_dir().join(format!(
        "jirc-file-lock-{}-{}",
        std::process::id(),
        NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&data_dir).unwrap();
    let ctx = RunCtx {
        server_id: "local",
        my_nick: "me",
        network: "Local",
        server: "",
        data_dir,
        state: Arc::new(Default::default()),
    };
    let owner = ScriptEngine::new();
    owner.load(
        "alias acquire {\n\
          fopen -ox held payload.bin\n\
          fwrite held ABCDEF\n\
          fseek held 0\n\
          echo -s owner=$fopen(held) error=$ferr bom=$fopen(held).bom\n\
        }\n\
        alias release { fclose held }",
    );
    assert_eq!(
        echoes(owner.run_alias(&ctx, "(status)", "acquire", "")),
        ["owner=held error=$false bom=ascii"]
    );
    let reader = ScriptEngine::new();
    reader.load(
        "alias check {\n\
          fopen other payload.bin\n\
          echo -s reader=$fopen(other) read=$fread(other) error=$ferr\n\
          fwrite other corrupt\n\
          echo -s write_error=$ferr position=$fopen(other).pos\n\
          fopen -o denied payload.bin\n\
          echo -s overwrite_error=$ferr denied=[$fopen(denied)] count=$fopen(0)\n\
          fclose *\n\
        }\n\
        alias writeafter {\n\
          fopen -ox new payload.bin\n\
          fwrite new released\n\
          echo -s released_error=$ferr\n\
          fclose new\n\
        }",
    );
    assert_eq!(
        echoes(reader.run_alias(&ctx, "(status)", "check", "")),
        [
            "reader=other read=ABCDEF error=$false",
            "write_error=$true position=6",
            "overwrite_error=$true denied=[] count=1",
        ]
    );
    assert_eq!(
        std::fs::read(ctx.data_dir.join("payload.bin")).unwrap(),
        b"ABCDEF"
    );
    owner.run_alias(&ctx, "(status)", "release", "");
    assert_eq!(
        echoes(reader.run_alias(&ctx, "(status)", "writeafter", "")),
        ["released_error=$false"]
    );
    assert_eq!(
        std::fs::read(ctx.data_dir.join("payload.bin")).unwrap(),
        b"released"
    );
    std::fs::remove_file(ctx.data_dir.join("payload.bin")).unwrap();
    std::fs::remove_dir(ctx.data_dir).unwrap();
}
