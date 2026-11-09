use super::*;
use crate::script::{RunCtx, ScriptEngine};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

async fn server(response: Vec<u8>) -> (String, tokio::task::JoinHandle<Vec<u8>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/request", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        loop {
            let mut bytes = [0; 4096];
            let count = stream.read(&mut bytes).await.unwrap();
            if count == 0 {
                break;
            }
            request.extend_from_slice(&bytes[..count]);
            if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("content-length:")
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if request.len() >= end + 4 + length {
                    break;
                }
            }
        }
        stream.write_all(&response).await.unwrap();
        request
    });
    (url, task)
}

fn register(engine: &ScriptEngine, ctx: &RunCtx<'_>, command: &str) -> (Arc<UrlGets>, u32) {
    let actions = engine.run_command(ctx, "#origin", command, &[]);
    let id = actions
        .iter()
        .find_map(|action| {
            if let Action::UrlGet { id } = action {
                Some(*id)
            } else {
                None
            }
        })
        .expect("request action");
    (engine.urlgets(), id)
}

async fn run(manager: &UrlGets, id: u32) -> (Request, Vec<u8>) {
    let request = {
        let mut state = manager.state.lock().unwrap();
        let entry = state.entries.get_mut(&id).unwrap();
        entry.running = true;
        entry.generation = 1;
        entry.started_at = Some(Instant::now());
        entry.request.clone()
    };
    let result = download(manager, id, 1, &request).await;
    let bytes = finish(manager, id, 1, &request, result.is_ok())
        .await
        .unwrap();
    (request, bytes)
}

#[test]
fn registration_is_visible_before_async_work_and_rejects_invalid_calls() {
    let engine = ScriptEngine::new();
    let (manager, id) = register(
        &engine,
        &context(),
        "noop $urlget(http://127.0.0.1:9/data,gb,&out,callback)",
    );
    assert_eq!(manager.property(0, ""), "1");
    assert_eq!(manager.property(id, "state"), "connect");
    assert_eq!(manager.property(1, "id"), id.to_string());
    assert_eq!(manager.property(id, "type"), "binvar");
    for call in [
        "$urlget(file:///outside,gf,out,callback)",
        "$urlget(http://127.0.0.1,gb,out,callback)",
        "$urlget(http://127.0.0.1,ggb,&out,callback)",
        "$urlget(http://127.0.0.1,gb,&out,bad callback)",
    ] {
        let actions = engine.run_command(&context(), "#c", &format!("echo -a {call}"), &[]);
        assert_eq!(
            actions,
            [Action::Echo {
                target: "(active)".into(),
                text: "0".into()
            }]
        );
    }
    manager.abandon(id);
    assert_eq!(manager.property(0, ""), "0");
}

#[test]
fn callback_keeps_file_local_alias_scope_and_never_falls_back_to_server_commands() {
    let engine = ScriptEngine::new();
    engine.load_sources(&[("a.mrc".into(), "alias start { noop $urlget(http://127.0.0.1:9/data,gb,&out,callback) }\nalias -l callback { echo -a local $1 $bvar(&out,0) }".into()),
        ("b.mrc".into(), "alias callback { echo -a wrong }".into())]);
    let actions = engine.run_alias(&context(), "#origin", "start", "");
    let id = actions
        .iter()
        .find_map(|action| {
            if let Action::UrlGet { id } = action {
                Some(*id)
            } else {
                None
            }
        })
        .unwrap();
    let request = engine.urlgets().state.lock().unwrap().entries[&id]
        .request
        .clone();
    assert_eq!(
        engine.run_urlget_callback(
            &context(),
            "#origin",
            &request.callback,
            id,
            &request.source,
            Some(("&out", b"abc"))
        ),
        [Action::Echo {
            target: "(active)".into(),
            text: format!("local {id} 3")
        }]
    );
    assert!(engine
        .run_urlget_callback(&context(), "#origin", "missing-callback", id, "a.mrc", None)
        .is_empty());
}

#[tokio::test]
async fn post_copies_binary_headers_and_body_and_exposes_callback_result() {
    let response = b"HTTP/1.1 200 OK\r\nX-Reply: received\r\nContent-Length: 3\r\nConnection: close\r\n\r\n\0\xffA".to_vec();
    let (url, capture) = server(response).await;
    let engine = ScriptEngine::new();
    engine.load("alias callback { echo -a $1 $urlget($1).state $urlget($1).rcvd $bvar(&out,0) $bvar(&out,1,3) }");
    let (manager, id) = register(&engine, &context(), &format!("bset -t &headers 1 X-Test: custom | bset &body 1 0 255 65 | noop $urlget({url},pb,&out,callback,&headers,&body)"));
    let (request, bytes) = run(&manager, id).await;
    let captured = capture.await.unwrap();
    assert!(captured.starts_with(b"POST /request HTTP/1.1\r\n"));
    assert!(String::from_utf8_lossy(&captured).contains("x-test: custom"));
    assert!(captured.ends_with(&[0, 255, 65]));
    assert_eq!(bytes, [0, 255, 65]);
    assert_eq!(manager.property(id, "state"), "ok");
    assert_eq!(manager.property(0, ""), "0");
    assert!(manager.property(id, "reply").contains("x-reply: received"));
    assert_eq!(
        engine.run_urlget_callback(
            &context(),
            "#origin",
            &request.callback,
            id,
            "",
            Some(("&out", &bytes))
        ),
        [Action::Echo {
            target: "(active)".into(),
            text: format!("{id} ok 3 3 0 255 65")
        }]
    );
    assert!(engine.inner.lock().unwrap().bins.get("&out").is_none());
}

#[tokio::test]
async fn resume_uses_range_and_restarts_when_server_ignores_it() {
    for partial in [true, false] {
        let response = if partial {
            b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 3-5/6\r\nContent-Length: 3\r\nConnection: close\r\n\r\ndef".to_vec()
        } else {
            b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nabcdef".to_vec()
        };
        let (url, capture) = server(response).await;
        let engine = ScriptEngine::new();
        let (manager, id) = register(
            &engine,
            &context(),
            &format!("noop $urlget({url},gbr,&out,callback)"),
        );
        manager
            .state
            .lock()
            .unwrap()
            .entries
            .get_mut(&id)
            .unwrap()
            .data = b"abc".to_vec();
        let (_, data) = run(&manager, id).await;
        assert_eq!(data, b"abcdef");
        assert_eq!(manager.property(id, "rcvd"), "6");
        let request = capture.await.unwrap();
        let headers = String::from_utf8_lossy(&request);
        assert!(headers.contains("range: bytes=3-"));
        assert!(headers.contains("accept-encoding: identity"));
    }
}

#[tokio::test]
async fn partial_range_cannot_be_reported_as_a_complete_download() {
    let (url, capture) = server(b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 3-5/9\r\nContent-Length: 3\r\nConnection: close\r\n\r\ndef".to_vec()).await;
    let engine = ScriptEngine::new();
    let (manager, id) = register(
        &engine,
        &context(),
        &format!("noop $urlget({url},gbr,&out,callback)"),
    );
    manager
        .state
        .lock()
        .unwrap()
        .entries
        .get_mut(&id)
        .unwrap()
        .data = b"abc".to_vec();
    let (_, data) = run(&manager, id).await;
    capture.await.unwrap();
    assert_eq!(data, b"abcdef");
    assert_eq!(manager.property(id, "size"), "9");
    assert_eq!(manager.property(id, "state"), "fail");
}

#[tokio::test]
async fn file_output_stays_in_script_sandbox_and_finishes_part_file() {
    let path = std::env::temp_dir().join(format!("jirc-http-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&path).unwrap();
    let mut ctx = context();
    ctx.data_dir = path.clone();
    let (url, capture) =
        server(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ndata".to_vec())
            .await;
    let engine = ScriptEngine::new();
    let (manager, id) = register(
        &engine,
        &ctx,
        &format!("noop $urlget({url},gft,../output.bin,callback)"),
    );
    run(&manager, id).await;
    capture.await.unwrap();
    assert_eq!(std::fs::read(path.join("output.bin")).unwrap(), b"data");
    assert!(!path.join("output.bin.part").exists());
    assert_eq!(manager.property(id, "state"), "ok");
    std::fs::remove_file(path.join("output.bin")).unwrap();
    std::fs::remove_dir(path).unwrap();
}

#[tokio::test]
async fn error_body_is_saved_only_with_explicit_error_option() {
    for save in [false, true] {
        let (url, capture) = server(
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 7\r\nConnection: close\r\n\r\nmissing"
                .to_vec(),
        )
        .await;
        let engine = ScriptEngine::new();
        let (manager, id) = register(
            &engine,
            &context(),
            &format!(
                "noop $urlget({url},gb{},&out,callback)",
                if save { "e" } else { "" }
            ),
        );
        let (_, bytes) = run(&manager, id).await;
        capture.await.unwrap();
        assert_eq!(manager.property(id, "state"), "fail");
        assert_eq!(
            bytes.as_slice(),
            if save {
                b"missing".as_slice()
            } else {
                b"".as_slice()
            }
        );
    }
}

#[tokio::test]
async fn head_retains_length_without_reading_a_body_and_redirect_policy_is_per_request() {
    let (url, capture) = server(
        b"HTTP/1.1 200 OK\r\nContent-Length: 1000000000\r\nConnection: close\r\n\r\n".to_vec(),
    )
    .await;
    let engine = ScriptEngine::new();
    let (manager, id) = register(
        &engine,
        &context(),
        &format!("noop $urlget({url},hb,&out,callback)"),
    );
    let (_, bytes) = run(&manager, id).await;
    assert!(bytes.is_empty());
    assert_eq!(manager.property(id, "size"), "1000000000");
    assert_eq!(manager.property(id, "state"), "ok");
    assert!(capture.await.unwrap().starts_with(b"HEAD "));

    for follow in [false, true] {
        let (destination, destination_capture) =
            server(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".to_vec())
                .await;
        let response = format!("HTTP/1.1 302 Found\r\nLocation: {destination}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        let (url, redirect_capture) = server(response.into_bytes()).await;
        let (manager, id) = register(
            &engine,
            &context(),
            &format!(
                "noop $urlget({url},gb{},&out,callback)",
                if follow { "" } else { "k" }
            ),
        );
        let (_, data) = run(&manager, id).await;
        redirect_capture.await.unwrap();
        assert_eq!(manager.property(id, "redirect"), destination);
        if follow {
            assert_eq!(data, b"ok");
            destination_capture.await.unwrap();
        } else {
            assert!(data.is_empty());
            destination_capture.abort();
        }
    }
}

#[tokio::test]
async fn obsolete_generation_cannot_publish_file_or_callback_payload() {
    let path = std::env::temp_dir().join(format!("jirc-http-stale-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&path).unwrap();
    let mut ctx = context();
    ctx.data_dir = path.clone();
    let engine = ScriptEngine::new();
    let (manager, id) = register(
        &engine,
        &ctx,
        "noop $urlget(http://127.0.0.1:9,gf,out.bin,callback)",
    );
    let request = {
        let mut state = manager.state.lock().unwrap();
        let entry = state.entries.get_mut(&id).unwrap();
        entry.generation = 2;
        entry.data = b"new".to_vec();
        entry.request.clone()
    };
    assert!(finish(&manager, id, 1, &request, true).await.is_none());
    assert!(!path.join("out.bin").exists());
    std::fs::remove_dir(path).unwrap();
}
