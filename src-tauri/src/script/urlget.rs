//! Portable, bounded asynchronous `$urlget` requests and completion callbacks.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use tauri::Manager;
use tokio::sync::Notify;

use super::eval::{Action, Runtime};

const MAX_BODY: usize = 32 * 1024 * 1024;
const MAX_TOTAL: usize = 128 * 1024 * 1024;
const MAX_ACTIVE: usize = 8;
const MAX_HISTORY: usize = 64;
const MAX_HEADERS: usize = 64 * 1024;

#[derive(Clone)]
struct Request {
    url: String,
    method: reqwest::Method,
    target: String,
    path: Option<PathBuf>,
    callback: String,
    source: String,
    reply_target: String,
    headers: HeaderMap,
    body: Vec<u8>,
    part: bool,
    resume: bool,
    insecure: bool,
    no_redirect: bool,
    save_errors: bool,
}

#[derive(Clone)]
struct Context {
    app: tauri::AppHandle,
    server_id: String,
    nick: String,
    network: String,
    server: String,
    target: String,
}

struct Entry {
    request: Request,
    context: Option<Context>,
    state: &'static str,
    redirect: String,
    reply: String,
    size: u64,
    received: u64,
    elapsed: u128,
    started_at: Option<Instant>,
    data: Vec<u8>,
    running: bool,
    completed: bool,
    callback_pending: bool,
    generation: u64,
    cancelled: bool,
    stop: Arc<Notify>,
}

#[derive(Default)]
struct State {
    next: u32,
    entries: BTreeMap<u32, Entry>,
}

impl State {
    fn make_room(&mut self, additional: usize, keep: Option<u32>) -> bool {
        loop {
            let used: usize = self
                .entries
                .values()
                .map(|entry| entry.data.len() + entry.request.body.len())
                .sum();
            if used.saturating_add(additional) <= MAX_TOTAL {
                return true;
            }
            let old = self
                .entries
                .iter()
                .find(|(id, entry)| {
                    Some(**id) != keep && entry.completed && !entry.callback_pending
                })
                .map(|(id, _)| *id);
            let Some(old) = old else { return false };
            self.entries.remove(&old);
        }
    }
}

#[derive(Default)]
pub struct UrlGets {
    state: Mutex<State>,
}

impl UrlGets {
    fn register(&self, request: Request) -> Option<u32> {
        let mut state = self.state.lock().unwrap();
        if state
            .entries
            .values()
            .filter(|entry| !entry.completed)
            .count()
            >= MAX_ACTIVE
        {
            return None;
        }
        while state.entries.len() >= MAX_HISTORY {
            let old = state
                .entries
                .iter()
                .find(|(_, entry)| entry.completed && !entry.callback_pending)
                .map(|(id, _)| *id)?;
            state.entries.remove(&old);
        }
        if !state.make_room(request.body.len(), None) {
            return None;
        }
        state.next = state.next.max(1023).checked_add(1)?;
        let id = state.next;
        state.entries.insert(
            id,
            Entry {
                request,
                context: None,
                state: "connect",
                redirect: String::new(),
                reply: String::new(),
                size: 0,
                received: 0,
                elapsed: 0,
                started_at: None,
                data: Vec::new(),
                running: false,
                completed: false,
                generation: 0,
                cancelled: false,
                callback_pending: false,
                stop: Arc::new(Notify::new()),
            },
        );
        Some(id)
    }

    fn select(state: &State, selector: u32) -> Option<u32> {
        if state.entries.contains_key(&selector) {
            Some(selector)
        } else {
            state
                .entries
                .iter()
                .filter(|(_, entry)| !entry.completed)
                .nth(selector.checked_sub(1)? as usize)
                .map(|(id, _)| *id)
        }
    }

    pub fn abandon(&self, id: u32) {
        let mut state = self.state.lock().unwrap();
        if state
            .entries
            .get(&id)
            .is_some_and(|entry| entry.generation == 0 && !entry.running)
        {
            state.entries.remove(&id);
        }
    }

    fn property(&self, selector: u32, property: &str) -> String {
        let state = self.state.lock().unwrap();
        if selector == 0 {
            return state
                .entries
                .values()
                .filter(|entry| !entry.completed)
                .count()
                .to_string();
        }
        let Some(id) = Self::select(&state, selector) else {
            return String::new();
        };
        let entry = &state.entries[&id];
        match property.to_ascii_lowercase().as_str() {
            "" | "id" => id.to_string(),
            "url" => entry.request.url.clone(),
            "redirect" => entry.redirect.clone(),
            "method" => entry.request.method.as_str().to_ascii_lowercase(),
            "type" => if entry.request.path.is_some() {
                "file"
            } else {
                "binvar"
            }
            .into(),
            "target" => entry.request.target.clone(),
            "alias" => entry.request.callback.clone(),
            "state" => entry.state.into(),
            "size" => entry.size.to_string(),
            "rcvd" => entry.received.to_string(),
            "resume" => u8::from(entry.request.resume).to_string(),
            "time" => entry
                .started_at
                .filter(|_| entry.running)
                .map(|at| at.elapsed().as_millis())
                .unwrap_or(entry.elapsed)
                .to_string(),
            "reply" => entry.reply.clone(),
            _ => String::new(),
        }
    }

    fn cancel(&self, id: u32) {
        let mut state = self.state.lock().unwrap();
        if let Some(entry) = state.entries.get_mut(&id) {
            entry.cancelled = true;
            entry.stop.notify_one();
        }
    }

    fn prepare_resume(&self, id: u32) -> bool {
        let mut state = self.state.lock().unwrap();
        let active = state
            .entries
            .values()
            .filter(|entry| !entry.completed)
            .count();
        let Some(entry) = state.entries.get_mut(&id) else {
            return false;
        };
        if entry.running && !entry.cancelled {
            return false;
        }
        if entry.completed && entry.state == "ok" {
            return false;
        }
        if entry.completed && active >= MAX_ACTIVE {
            return false;
        }
        entry.stop.notify_one();
        entry.generation = entry.generation.wrapping_add(1);
        entry.stop = Arc::new(Notify::new());
        entry.request.resume = true;
        entry.running = false;
        entry.cancelled = false;
        entry.completed = false;
        entry.callback_pending = false;
        entry.state = "connect";
        true
    }
}

fn parse_request(rt: &Runtime<'_>, args: &[String]) -> Option<Request> {
    let url = args.first()?.trim();
    let parsed = reqwest::Url::parse(url).ok()?;
    if url.chars().count() > 2000 || !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let options = args.get(1)?.to_ascii_lowercase();
    if options.chars().any(|flag| !"hgpuadfbrtike".contains(flag)) {
        return None;
    }
    let methods = options
        .chars()
        .filter(|flag| "hgpuad".contains(*flag))
        .collect::<Vec<_>>();
    if methods.len() != 1 || options.contains('f') == options.contains('b') {
        return None;
    }
    let method = match methods[0] {
        'h' => reqwest::Method::HEAD,
        'g' => reqwest::Method::GET,
        'p' => reqwest::Method::POST,
        'u' => reqwest::Method::PUT,
        'a' => reqwest::Method::PATCH,
        'd' => reqwest::Method::DELETE,
        _ => return None,
    };
    let target = args.get(2)?.trim().trim_matches('"').to_string();
    if target.is_empty() || (options.contains('b') && !target.starts_with('&')) {
        return None;
    }
    let callback = args.get(3)?.trim();
    if callback.is_empty()
        || callback
            .chars()
            .any(|c| !c.is_alphanumeric() && !"_.-[]".contains(c))
    {
        return None;
    }
    let mut headers = HeaderMap::new();
    if let Some(name) = args.get(4).filter(|value| !value.is_empty()) {
        let bytes = rt.bins.get(name)?;
        if bytes.len() > MAX_HEADERS {
            return None;
        }
        for line in bytes.split(|byte| *byte == b'\n') {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if line.is_empty() {
                continue;
            }
            let colon = line.iter().position(|byte| *byte == b':')?;
            let name = HeaderName::from_bytes(&line[..colon]).ok()?;
            let value = line[colon + 1..]
                .strip_prefix(b" ")
                .unwrap_or(&line[colon + 1..]);
            headers.append(name, HeaderValue::from_bytes(value).ok()?);
        }
    }
    let body = match args.get(5).filter(|value| !value.is_empty()) {
        Some(name) => rt.bins.get(name)?.clone(),
        None => Vec::new(),
    };
    if body.len() > MAX_BODY {
        return None;
    }
    let path = options
        .contains('f')
        .then(|| super::eval::sandbox_path(&rt.data_dir, &target));
    Some(Request {
        url: url.into(),
        method,
        target,
        path,
        callback: callback.into(),
        source: rt.event.script_source.clone(),
        headers,
        body,
        reply_target: rt.event.target.clone(),
        part: options.contains('t'),
        resume: options.contains('r'),
        insecure: options.contains('i'),
        no_redirect: options.contains('k'),
        save_errors: options.contains('e'),
    })
}

pub fn identifier(rt: &mut Runtime<'_>, args: &[String], property: &str) -> String {
    if let Some(selector) = args.first().and_then(|value| value.parse::<u32>().ok()) {
        if let Some(control) = args.get(1) {
            let id = {
                let state = rt.urlgets.state.lock().unwrap();
                UrlGets::select(&state, selector)
            };
            let Some(id) = id else { return "0".into() };
            if control.eq_ignore_ascii_case("c") || control.eq_ignore_ascii_case("r") {
                rt.actions.push(Action::UrlGetControl {
                    id,
                    resume: control.eq_ignore_ascii_case("r"),
                });
                return id.to_string();
            }
            return "0".into();
        }
        return rt.urlgets.property(selector, property);
    }
    let Some(request) = parse_request(rt, args) else {
        return "0".into();
    };
    let Some(id) = rt.urlgets.register(request) else {
        return "0".into();
    };
    rt.actions.push(Action::UrlGet { id });
    id.to_string()
}

pub struct Activation(Arc<Notify>);
impl Drop for Activation {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}

struct CallbackLease {
    manager: Arc<UrlGets>,
    id: u32,
    generation: u64,
}
impl Drop for CallbackLease {
    fn drop(&mut self) {
        if let Some(entry) = self.manager.state.lock().unwrap().entries.get_mut(&self.id) {
            if entry.generation == self.generation {
                entry.callback_pending = false;
            }
        }
    }
}

pub fn activate(
    app: &tauri::AppHandle,
    server_id: &str,
    nick: &str,
    network: &str,
    server: &str,
    id: u32,
    resume: Option<bool>,
) -> Option<Activation> {
    let engine = app.try_state::<super::ScriptEngine>()?;
    let manager = engine.urlgets();
    if resume == Some(false) {
        manager.cancel(id);
        return None;
    }
    if resume == Some(true) && !manager.prepare_resume(id) {
        return None;
    }
    let (request, context, generation, stop) = {
        let mut state = manager.state.lock().unwrap();
        let entry = state.entries.get_mut(&id)?;
        if entry.running || entry.completed {
            return None;
        }
        let context = entry
            .context
            .get_or_insert_with(|| Context {
                app: app.clone(),
                server_id: server_id.into(),
                nick: nick.into(),
                network: network.into(),
                server: server.into(),
                target: entry.request.reply_target.clone(),
            })
            .clone();
        entry.running = true;
        entry.generation = entry.generation.wrapping_add(1);
        entry.started_at = Some(Instant::now());
        (
            entry.request.clone(),
            context,
            entry.generation,
            entry.stop.clone(),
        )
    };
    let gate = Arc::new(Notify::new());
    let task_gate = gate.clone();
    tauri::async_runtime::spawn(async move {
        task_gate.notified().await;
        let result = tokio::select! {
            biased;
            _ = stop.notified() => Err("cancelled".to_string()),
            result = download(&manager, id, generation, &request) => result,
        };
        let payload = finish(&manager, id, generation, &request, result.is_ok()).await;
        let Some(payload) = payload else { return };
        let lease = CallbackLease {
            manager: manager.clone(),
            id,
            generation,
        };
        // Script evaluation and file output run off the async network executor.
        let _ = tauri::async_runtime::spawn_blocking(move || {
            let _lease = lease;
            let Some(engine) = context.app.try_state::<super::ScriptEngine>() else {
                return;
            };
            let state = context
                .app
                .try_state::<crate::irc::state::StateStore>()
                .map(|store| store.get(&context.server_id))
                .unwrap_or_default();
            let nick = if state.nick.is_empty() {
                context.nick.clone()
            } else {
                state.nick.clone()
            };
            let ctx = super::RunCtx {
                server_id: &context.server_id,
                my_nick: &nick,
                network: &context.network,
                server: &context.server,
                data_dir: super::script_data_dir(&context.app),
                state,
            };
            let bin = request
                .path
                .is_none()
                .then_some((request.target.as_str(), payload.as_slice()));
            let actions = engine.run_urlget_callback(
                &ctx,
                &context.target,
                &request.callback,
                id,
                &request.source,
                bin,
            );
            super::apply_actions(
                &context.app,
                &context.server_id,
                &nick,
                &context.network,
                &context.server,
                actions,
            );
        })
        .await;
    });
    Some(Activation(gate))
}

fn output_path(request: &Request) -> Option<PathBuf> {
    request.path.as_ref().map(|path| {
        if request.part {
            PathBuf::from(format!("{}.part", path.to_string_lossy()))
        } else {
            path.clone()
        }
    })
}

async fn download(
    manager: &UrlGets,
    id: u32,
    generation: u64,
    request: &Request,
) -> Result<(), String> {
    let mut offset = {
        let mut state = manager.state.lock().unwrap();
        let entry = state.entries.get_mut(&id).ok_or("request removed")?;
        if !request.resume {
            entry.data.clear();
        }
        entry.data.len()
    };
    if request.resume && offset == 0 {
        if let Some(path) = output_path(request) {
            let data = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
                use std::io::Read;
                let Ok(file) = std::fs::File::open(path) else {
                    return Ok(Vec::new());
                };
                let mut data = Vec::new();
                file.take((MAX_BODY + 1) as u64)
                    .read_to_end(&mut data)
                    .map_err(|e| e.to_string())?;
                if data.len() > MAX_BODY {
                    return Err("resume file exceeds size limit".into());
                }
                Ok(data)
            })
            .await
            .map_err(|e| e.to_string())??;
            offset = data.len();
            let mut state = manager.state.lock().unwrap();
            if !state.make_room(data.len(), Some(id)) {
                return Err("HTTP memory limit".into());
            }
            let entry = state.entries.get_mut(&id).ok_or("request removed")?;
            if entry.generation != generation {
                return Err("superseded".into());
            }
            entry.data = data;
        }
    }
    let policy = if request.no_redirect {
        reqwest::redirect::Policy::none()
    } else {
        reqwest::redirect::Policy::limited(10)
    };
    let client = reqwest::Client::builder()
        .redirect(policy)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        // Byte ranges refer to the stored representation. Do not combine a
        // decoded partial response with a prefix in a different representation.
        .gzip(offset == 0)
        .brotli(offset == 0)
        .deflate(offset == 0)
        .tls_danger_accept_invalid_certs(request.insecure)
        .build()
        .map_err(|e| e.to_string())?;
    let mut headers = request.headers.clone();
    if offset > 0 {
        headers.insert(
            reqwest::header::ACCEPT_ENCODING,
            HeaderValue::from_static("identity"),
        );
    }
    let mut pending = client
        .request(request.method.clone(), &request.url)
        .headers(headers)
        .body(request.body.clone());
    if offset > 0 {
        pending = pending.header(reqwest::header::RANGE, format!("bytes={offset}-"));
    }
    let mut response = pending.send().await.map_err(|e| e.to_string())?;
    let status = response.status();
    let partial = status == reqwest::StatusCode::PARTIAL_CONTENT;
    let partial_total = if partial {
        let range = response
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("bytes "))
            .and_then(|value| value.split_once('/'));
        let (start, end, total) = range
            .and_then(|(range, total)| {
                let (start, end) = range.split_once('-')?;
                Some((
                    start.parse::<u64>().ok()?,
                    end.parse::<u64>().ok()?,
                    if total == "*" {
                        None
                    } else {
                        Some(total.parse::<u64>().ok()?)
                    },
                ))
            })
            .ok_or("invalid Content-Range")?;
        if start != offset as u64 || end < start || total.is_some_and(|total| end >= total) {
            return Err("invalid Content-Range".into());
        }
        total
    } else {
        None
    };
    let mut reply = format!("{:?} {}\r\n", response.version(), status);
    for (name, value) in response.headers() {
        if reply.len() + name.as_str().len() + value.as_bytes().len() + 4 > MAX_HEADERS {
            return Err("response headers exceed limit".into());
        }
        reply.push_str(name.as_str());
        reply.push_str(": ");
        reply.push_str(&String::from_utf8_lossy(value.as_bytes()));
        reply.push_str("\r\n");
    }
    {
        let mut state = manager.state.lock().unwrap();
        let entry = state.entries.get_mut(&id).ok_or("request removed")?;
        if entry.generation != generation {
            return Err("superseded".into());
        }
        if !partial {
            entry.data.clear();
            offset = 0;
        }
        entry.state = "download";
        entry.redirect = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
            .unwrap_or_else(|| {
                if response.url().as_str() != request.url {
                    response.url().to_string()
                } else {
                    String::new()
                }
            });
        entry.reply = reply;
        entry.size = partial_total.unwrap_or_else(|| {
            response
                .headers()
                .get(reqwest::header::CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok())
                .or_else(|| response.content_length())
                .unwrap_or(0)
                .saturating_add(offset as u64)
        });
        entry.received = entry.data.len() as u64;
        if request.method != reqwest::Method::HEAD && entry.size > MAX_BODY as u64 {
            return Err("response exceeds size limit".into());
        }
    }
    if !status.is_success() && !request.save_errors {
        return Err(format!("HTTP {status}"));
    }
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        let mut state = manager.state.lock().unwrap();
        if !state.make_room(chunk.len(), Some(id)) {
            return Err("HTTP memory limit".into());
        }
        let entry = state.entries.get_mut(&id).ok_or("request removed")?;
        if entry.generation != generation {
            return Err("superseded".into());
        }
        if entry.data.len().saturating_add(chunk.len()) > MAX_BODY {
            return Err("response exceeds size limit".into());
        }
        entry.data.extend_from_slice(&chunk);
        entry.received = entry.data.len() as u64;
    }
    if let Some(total) = partial_total {
        let state = manager.state.lock().unwrap();
        if state
            .entries
            .get(&id)
            .is_none_or(|entry| entry.received != total)
        {
            return Err("incomplete resumed response".into());
        }
    }
    if status.is_success() {
        Ok(())
    } else {
        Err(format!("HTTP {status}"))
    }
}

async fn finish(
    manager: &UrlGets,
    id: u32,
    generation: u64,
    request: &Request,
    mut ok: bool,
) -> Option<Vec<u8>> {
    let data = {
        let state = manager.state.lock().unwrap();
        let entry = state.entries.get(&id)?;
        if entry.generation != generation {
            return None;
        }
        entry.data.clone()
    };
    if let Some(path) = output_path(request).filter(|_| ok || !data.is_empty()) {
        let final_path = request.path.clone().unwrap();
        let temporary = PathBuf::from(format!(
            "{}.jirc-{id}-{generation}.tmp",
            path.to_string_lossy()
        ));
        let bytes = data.clone();
        let part = request.part;
        let write_result = tokio::task::spawn_blocking(
            move || -> std::io::Result<(PathBuf, PathBuf, PathBuf, bool)> {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&temporary, bytes)?;
                Ok((temporary, path, final_path, part))
            },
        )
        .await;
        if let Ok(Ok((temporary, path, final_path, part))) = write_result {
            // Publish only a still-current generation. A resumed request must
            // never have its output replaced by the old cancelled worker.
            let state = manager.state.lock().unwrap();
            let current = state
                .entries
                .get(&id)
                .is_some_and(|entry| entry.generation == generation);
            if !current {
                let _ = std::fs::remove_file(temporary);
                return None;
            }
            ok &= std::fs::rename(&temporary, &path).is_ok();
            if ok && part {
                ok &= std::fs::rename(&path, &final_path).is_ok();
            }
            let _ = std::fs::remove_file(temporary);
        } else {
            ok = false;
        }
    }
    let mut state = manager.state.lock().unwrap();
    let entry = state.entries.get_mut(&id)?;
    if entry.generation != generation {
        return None;
    }
    entry.running = false;
    entry.completed = true;
    entry.callback_pending = true;
    entry.state = if ok { "ok" } else { "fail" };
    entry.elapsed = entry
        .started_at
        .map(|at| at.elapsed().as_millis())
        .unwrap_or(0);
    if ok {
        entry.data.clear();
    }
    Some(data)
}

#[cfg(test)]
#[path = "urlget_tests.rs"]
mod tests;
