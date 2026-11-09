//! Folders inside the script sandbox (TODO X-01, decision D-04 option B).
//! Expected values are what mIRC 7.79 printed for the same lines, relative to
//! its own `$mircdir`; see `analysis/rea-paths-20261109/native-fp.txt`.

use super::{eval::Action, RunCtx, ScriptEngine};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

fn fixture() -> RunCtx<'static> {
    let data_dir = std::env::temp_dir().join(format!(
        "jirc-sandbox-folders-{}-{}",
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

/// Runs `body` as alias `t` in a fresh sandbox and returns what `out` printed
/// (or the error line).
fn run(body: &str) -> Vec<String> {
    let ctx = fixture();
    let engine = ScriptEngine::new();
    engine.load(&format!(
        "alias out {{ echo -a < $+ $1- $+ > }}\nalias t {{\n  {body}\n}}"
    ));
    let lines = engine
        .run_alias(&ctx, "#c", "t", "")
        .into_iter()
        .filter_map(|action| match action {
            Action::Echo { text, .. } => Some(text),
            _ => None,
        })
        .collect();
    let _ = std::fs::remove_dir_all(&ctx.data_dir);
    lines
}

#[test]
fn relative_paths_keep_their_folders() {
    for (body, expected) in [
        (
            "mkdir fa02 | write -c fa02\\x.txt hi | out $isfile(fa02\\x.txt) $isfile(x.txt) $read(fa02/x.txt,1) $lines(fa02\\x.txt)",
            "<$true $false hi 1>",
        ), // f02
        (
            "mkdir fa03\\sub | out $isdir(fa03\\sub) $isdir(fa03) $isdir(fa03/sub)",
            "<$true $true $true>",
        ), // f03
        (
            "mkdir fa04 | write -c fa04\\a.txt A | out $nopath($findfile(fa04,*.txt,1)) $read($findfile(fa04,*.txt,1),1)",
            "<a.txt A>",
        ), // f04
        (
            "mkdir fa06 | write -c fa06\\a.txt A | write -c a06.txt B | out $read(fa06\\a.txt,1) $read(a06.txt,1) $isfile(fa06\\a06.txt)",
            "<A B $false>",
        ), // f06
        (
            "mkdir fa07 | write -c fa07\\..\\r07.txt C | out $read(r07.txt,1) $isfile(fa07\\r07.txt)",
            "<C $false>",
        ), // f07
        (
            "mkdir fa08 | write -c fa08\\x.txt z | remove fa08\\x.txt | out $isfile(fa08\\x.txt) $isdir(fa08)",
            "<$false $true>",
        ), // f08
        (
            "mkdir fa09 | writeini fa09\\s.ini sec k v | out $readini(fa09\\s.ini,sec,k) $isfile(s.ini) $ini(fa09\\s.ini,0)",
            "<v $false 1>",
        ), // f09
        (
            "mkdir fa10 | write -c fa10/f.txt fw | out $read(fa10\\f.txt,1)",
            "<fw>",
        ), // f10
        (
            "mkdir fa11 | write -c fa11\\x.txt hi | copy fa11\\x.txt fa11\\y.txt | rename fa11\\y.txt fa11\\z.txt | out $isfile(fa11\\y.txt) $isfile(fa11\\z.txt) $read(fa11\\z.txt,1)",
            "<$false $true hi>",
        ), // f11
        (
            "mkdir fa13\\sub | rmdir fa13\\sub | out $isdir(fa13\\sub) $isdir(fa13)",
            "<$false $true>",
        ), // f13
        (
            "mkdir fa14 | mkdir fa14\\in | write -c fa14\\in\\q.txt Q | out $findfile(fa14,*.txt,0) $nopath($findfile(fa14,*.txt,1)) $finddir(fa14,*,0)",
            "<1 q.txt 1>",
        ), // f14
        (
            "mkdir fa15 | write -c fa15\\x.txt one | out $read(.\\fa15\\x.txt,1) $read(fa15\\.\\x.txt,1) $read(fa15\\\\x.txt,1)",
            "<one one one>",
        ), // f15
        (
            "mkdir fa16 | write -c fa16\\x.txt one | fopen h16 fa16\\x.txt | out $fread(h16) $nopath($fopen(h16).fname) | fclose h16",
            "<one x.txt>",
        ), // f16
        (
            "mkdir fa17 | write -c fa17\\x.txt hi | out $exists(fa17\\x.txt) $exists(fa17) $exists(fa17\\nope.txt)",
            "<$true $true $false>",
        ), // f17
        (
            "mkdir fa19 | write -c fa19\\x.txt hi | out $isfile($mircdir $+ fa19\\x.txt)",
            "<$true>",
        ), // f19 (first half)
        (
            "out $isdir(nope20) $isfile(nope20\\x.txt) $read(nope20\\x.txt,1) $lines(nope20\\x.txt)",
            "<$false $false 0>",
        ), // f20
        ("out $isdir($mircdir)", "<$true>"), // f05 (first half)
        (
            "mkdir sp1 | write -c sp1\\file.txt a | write -c file.txt b | out $samepath(file.txt,sp1\\file.txt) $samepath(sp1\\..\\file.txt,file.txt) $samepath(file.txt,other.txt)",
            "<$false $true $false>",
        ), // native-sp s1
        (
            "out $samepath(file.txt,nested/file.txt) $samepath(nested/a.txt,nested\\a.txt)",
            "<$false $true>",
        ), // native-sp s2
        (
            "mkdir sp3 | write -c sp3\\x.txt hello | out $file(sp3\\x.txt).size $lines(sp3\\x.txt)",
            "<7 1>",
        ), // native-sp s3
    ] {
        assert_eq!(run(body), [expected], "{body}");
    }
}

#[test]
fn write_into_a_missing_folder_is_an_error() {
    // f01: mIRC raises `* /write: unable to open file '<path>'`; no folder is made.
    let lines = run("write -c fa01\\x.txt hi | out never");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].starts_with("* /write: unable to open file '"),
        "{lines:?}"
    );
}

#[test]
fn paths_outside_the_sandbox_keep_only_their_file_name() {
    // An absolute path that isn't the sandbox's own stays confined to its file
    // name (`file_identifier` relies on this); `..` never leaves the sandbox.
    for (body, expected) in [
        (
            "write -c probe.dat hello | out $isfile(C:\\anywhere\\probe.dat) $isfile(/elsewhere/probe.dat)",
            "<$true $true>",
        ),
        (
            "write -c up.txt U | out $read(..\\..\\up.txt,1) $read(a\\..\\..\\up.txt,1)",
            "<U U>",
        ),
    ] {
        assert_eq!(run(body), [expected], "{body}");
    }
}

#[test]
fn a_missing_folder_is_each_commands_error() {
    // Native mIRC 7.79 messages (`native-vp-fold.txt` v10, v21, v24, v37, v39, v61).
    for (body, start) in [
        (
            "write -c c.txt c | copy c.txt nod\\c.txt | out never",
            "* /copy: unable to copy '",
        ),
        (
            "write -c c.txt c | copy -a c.txt nod\\c.txt | out never",
            "* /copy: unable to open file '",
        ),
        (
            "write -c r.txt r | rename r.txt nod\\r.txt | out never",
            "* /rename: unable to rename '",
        ),
        (
            "write -c in.txt apple | filter -ff in.txt nod\\out.txt * | out never",
            "* /filter: unable to open file '",
        ),
        (
            "hmake t | hadd t k v | hsave t nod\\h.txt | out never",
            "* /hsave: unable to open file '",
        ),
        (
            "bset -t &b 1 hello | bwrite nod\\b.dat 0 -1 &b | out never",
            "* /bwrite: unable to open file '",
        ),
    ] {
        let lines = run(body);
        assert!(
            lines.len() == 1 && lines[0].starts_with(start),
            "{body}: {lines:?}"
        );
    }
}

#[test]
fn a_trailing_separator_names_a_different_path() {
    // native-vp-fold v56
    assert_eq!(
        run("mkdir vq56 | write -c vq56\\a.txt 1 | out $samepath(vq56\\a.txt,vq56/a.txt) $samepath(vq56,vq56\\)"),
        ["<$true $false>"]
    );
}

#[test]
fn a_colon_inside_a_path_cannot_name_a_drive() {
    let dir = std::path::Path::new("base");
    assert_eq!(
        super::eval::sandbox_path(dir, "a\\D:x.txt"),
        dir.join("a").join("D_x.txt")
    );
    assert_eq!(
        super::eval::sandbox_path(dir, "C:\\x\\D:y.txt"),
        dir.join("D_y.txt")
    );
}

/// Makes a sandbox with `outside` beside it and runs `setup` to place links.
#[cfg(unix)]
fn linked_run(
    setup: impl Fn(&std::path::Path, &std::path::Path),
    body: &str,
) -> (Vec<String>, std::path::PathBuf, std::path::PathBuf) {
    let ctx = fixture();
    let outside = ctx.data_dir.with_extension("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "outside").unwrap();
    setup(&ctx.data_dir, &outside);
    let engine = ScriptEngine::new();
    engine.load(&format!(
        "alias out {{ echo -a < $+ $1- $+ > }}\nalias t {{\n  {body}\n}}"
    ));
    let lines = engine
        .run_alias(&ctx, "#c", "t", "")
        .into_iter()
        .filter_map(|action| match action {
            Action::Echo { text, .. } => Some(text),
            _ => None,
        })
        .collect();
    (lines, ctx.data_dir, outside)
}

#[cfg(unix)]
fn clean(data: &std::path::Path, outside: &std::path::Path) {
    let _ = std::fs::remove_dir_all(data);
    let _ = std::fs::remove_dir_all(outside);
}

#[cfg(unix)]
#[test]
fn links_inside_the_sandbox_are_never_followed() {
    use std::os::unix::fs::symlink;
    // A link to an outside folder, as a folder on the way.
    let (_, data, outside) = linked_run(
        |data, outside| symlink(outside, data.join("link")).unwrap(),
        "write -c link\\x.txt hi",
    );
    assert!(!outside.join("x.txt").exists() && !data.join("x.txt").exists());
    clean(&data, &outside);
    // A dangling link, as the file and as a folder on the way.
    let (_, data, outside) = linked_run(
        |data, outside| {
            std::fs::create_dir_all(data.join("sub")).unwrap();
            symlink(outside.join("new.txt"), data.join("sub").join("d.txt")).unwrap();
            symlink(outside.join("newdir"), data.join("sub").join("dd")).unwrap();
        },
        "write -c sub\\d.txt X | mkdir sub\\dd | write -c sub\\dd\\y.txt Y | copy -o sub\\x sub\\d.txt",
    );
    assert!(!outside.join("new.txt").exists() && !outside.join("newdir").exists());
    clean(&data, &outside);
    // A link as the file itself, read and written, also as `$mircdir` + name.
    let (lines, data, outside) = linked_run(
        |data, outside| symlink(outside.join("secret.txt"), data.join("lfile.txt")).unwrap(),
        "out $read(lfile.txt,1) $isfile(lfile.txt) $read($mircdir $+ lfile.txt,1) | write -c lfile.txt changed",
    );
    assert_eq!(lines[0], "<$false>");
    assert_eq!(
        std::fs::read_to_string(outside.join("secret.txt")).unwrap(),
        "outside"
    );
    clean(&data, &outside);
    // `$findfile` neither lists nor follows a linked folder, nor loops on one.
    let (lines, data, outside) = linked_run(
        |data, outside| {
            std::fs::create_dir_all(data.join("sub")).unwrap();
            symlink(outside, data.join("sub").join("ldir")).unwrap();
            symlink("..", data.join("lup")).unwrap();
            symlink("../..", data.join("sub").join("lup2")).unwrap();
        },
        "out $findfile($mircdir,*,0) $finddir($mircdir,*,0)",
    );
    assert_eq!(lines, ["<0 1>"]);
    clean(&data, &outside);
    // `$zip` neither packs a linked file or folder nor extracts through a link.
    let (_, data, outside) = linked_run(
        |data, outside| {
            std::fs::create_dir_all(data.join("pack")).unwrap();
            std::fs::write(data.join("pack").join("own.txt"), "own").unwrap();
            symlink(outside, data.join("pack").join("ldir")).unwrap();
            symlink(
                outside.join("secret.txt"),
                data.join("pack").join("lfile.txt"),
            )
            .unwrap();
            symlink("..", data.join("pack").join("lup")).unwrap();
            std::fs::create_dir_all(data.join("dest")).unwrap();
            symlink(outside, data.join("dest").join("pack")).unwrap();
        },
        "noop $zip(arc.zip,c,pack) | noop $zip(arc.zip,eo,dest) | noop $zip(arc.zip,e,dest2)",
    );
    let names: Vec<String> = {
        let file = std::fs::File::open(data.join("arc.zip")).unwrap();
        let archive = zip::ZipArchive::new(file).unwrap();
        archive.file_names().map(str::to_string).collect()
    };
    assert!(
        names.iter().any(|name| name.ends_with("own.txt")),
        "{names:?}"
    );
    assert!(
        !names
            .iter()
            .any(|name| name.contains("ldir") || name.contains("lfile") || name.contains("lup")),
        "{names:?}"
    );
    assert!(!outside.join("own.txt").exists());
    assert!(data.join("dest2").join("pack").join("own.txt").exists());
    clean(&data, &outside);
}

#[cfg(unix)]
#[test]
fn zip_extraction_resolves_dot_dot_before_the_link_check() {
    use std::io::Write;
    use std::os::unix::fs::symlink;
    let (_, data, outside) = linked_run(
        |data, outside| {
            std::fs::create_dir_all(data.join("dest")).unwrap();
            symlink(outside, data.join("dest").join("pack")).unwrap();
            let file = std::fs::File::create(data.join("crafted.zip")).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file("missing/../pack/landed.txt", options)
                .unwrap();
            zip.write_all(b"x").unwrap();
            zip.finish().unwrap();
        },
        "noop $zip(crafted.zip,eo,dest)",
    );
    assert!(!outside.join("landed.txt").exists());
    let _ = std::fs::remove_dir_all(&data);
    let _ = std::fs::remove_dir_all(&outside);
}

#[test]
fn zip_entry_names_get_the_same_colon_rule() {
    use std::io::Write;
    let ctx = fixture();
    let file = std::fs::File::create(ctx.data_dir.join("c.zip")).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    zip.start_file("fl:x", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"x").unwrap();
    zip.finish().unwrap();
    let engine = ScriptEngine::new();
    engine.load("alias t { noop $zip(c.zip,e,dest) }");
    engine.run_alias(&ctx, "#c", "t", "");
    assert!(ctx.data_dir.join("dest").join("fl_x").exists());
    let _ = std::fs::remove_dir_all(&ctx.data_dir);
}
