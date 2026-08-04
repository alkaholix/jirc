//! Sandboxed cross-platform Lua plugins.
//!
//! Each `plugins/*.lua` file returns a table containing `name` and an optional
//! `on_event(event, payload)` function. The function may return an array of
//! validated actions and a handled flag. Luau's sandbox removes host access;
//! jIRC deliberately exposes no filesystem, process, network, or native APIs.

use std::{
    collections::HashSet,
    fs,
    path::Path,
    time::{Duration, Instant},
};

use mlua::{Function, Lua, LuaSerdeExt, Table, Value, VmState};
use serde::{Deserialize, Serialize};
use tauri::AppHandle;

const DISABLED_FILE: &str = "_disabled.json";
const MAX_SOURCE_BYTES: u64 = 256 * 1024;
const MAX_ACTIONS: usize = 32;
const MEMORY_LIMIT: usize = 4 * 1024 * 1024;
const TIME_LIMIT: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginInfo {
    pub file: String,
    pub name: String,
    pub enabled: bool,
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum PluginAction {
    Echo { target: String, text: String },
    Command { command: String },
    Notify { title: String, text: String },
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginDispatch {
    pub actions: Vec<PluginAction>,
    pub handled: bool,
    pub errors: Vec<String>,
}

fn safe_file(name: &str) -> Result<String, String> {
    let file = Path::new(name)
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| "invalid plugin filename".to_string())?;
    if file != name || !file.to_ascii_lowercase().ends_with(".lua") {
        return Err("plugin must be a .lua file in the plugins folder".into());
    }
    Ok(file.to_string())
}

fn disabled(dir: &Path) -> HashSet<String> {
    fs::read_to_string(dir.join(DISABLED_FILE))
        .ok()
        .and_then(|text| serde_json::from_str::<Vec<String>>(&text).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|name| name.to_ascii_lowercase())
        .collect()
}

fn save_disabled(dir: &Path, values: &HashSet<String>) -> Result<(), String> {
    let mut values = values.iter().cloned().collect::<Vec<_>>();
    values.sort();
    fs::write(
        dir.join(DISABLED_FILE),
        serde_json::to_string_pretty(&values).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}

fn runtime(source: &str) -> Result<(Lua, String), String> {
    let lua = Lua::new();
    lua.set_memory_limit(MEMORY_LIMIT)
        .map_err(|error| error.to_string())?;
    for name in [
        "os", "io", "package", "debug", "require", "loadfile", "dofile",
    ] {
        lua.globals()
            .set(name, Value::Nil)
            .map_err(|error| error.to_string())?;
    }
    lua.sandbox(true).map_err(|error| error.to_string())?;
    let deadline = Instant::now() + TIME_LIMIT;
    lua.set_interrupt(move |_| {
        if Instant::now() >= deadline {
            Err(mlua::Error::RuntimeError(
                "plugin execution time limit exceeded".into(),
            ))
        } else {
            Ok(VmState::Continue)
        }
    });
    let plugin: Table = lua
        .load(source)
        .set_name("plugin")
        .eval()
        .map_err(|error| error.to_string())?;
    let name = plugin
        .get::<Option<String>>("name")
        .map_err(|error| error.to_string())?
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "Unnamed plugin".into());
    lua.set_named_registry_value("jirc_plugin", plugin)
        .map_err(|error| error.to_string())?;
    Ok((lua, name))
}

fn run(
    source: &str,
    event: &str,
    payload: &serde_json::Value,
) -> Result<(Vec<PluginAction>, bool), String> {
    let (lua, _) = runtime(source)?;
    let plugin: Table = lua
        .named_registry_value("jirc_plugin")
        .map_err(|error| error.to_string())?;
    let Some(callback) = plugin
        .get::<Option<Function>>("on_event")
        .map_err(|error| error.to_string())?
    else {
        return Ok((Vec::new(), false));
    };
    let payload = lua.to_value(payload).map_err(|error| error.to_string())?;
    let (value, handled): (Value, Option<bool>) = callback
        .call((event, payload))
        .map_err(|error| error.to_string())?;
    let mut actions = if matches!(value, Value::Nil) {
        Vec::new()
    } else {
        lua.from_value::<Vec<PluginAction>>(value)
            .map_err(|error| format!("invalid plugin actions: {error}"))?
    };
    if actions.len() > MAX_ACTIONS {
        actions.truncate(MAX_ACTIONS);
    }
    actions.retain(valid_action);
    Ok((actions, handled.unwrap_or(false)))
}

fn valid_action(action: &PluginAction) -> bool {
    match action {
        PluginAction::Echo { target, text } => {
            target.len() <= 128 && !text.is_empty() && text.len() <= 4096
        }
        PluginAction::Command { command } => command.starts_with('/') && command.len() <= 4096,
        PluginAction::Notify { title, text } => {
            !title.is_empty() && title.len() <= 256 && text.len() <= 4096
        }
    }
}

#[tauri::command]
pub fn plugins_list(app: AppHandle) -> Result<Vec<PluginInfo>, String> {
    let dir = crate::storage::plugins_dir(&app)?;
    let disabled = disabled(&dir);
    let mut result = Vec::new();
    for entry in fs::read_dir(&dir).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let file = entry.file_name().to_string_lossy().into_owned();
        if !entry
            .file_type()
            .map_err(|error| error.to_string())?
            .is_file()
            || !file.to_ascii_lowercase().ends_with(".lua")
        {
            continue;
        }
        let (name, error) = match fs::read_to_string(entry.path())
            .map_err(|error| error.to_string())
            .and_then(|source| runtime(&source).map(|(_, name)| name))
        {
            Ok(name) => (name, String::new()),
            Err(error) => (file.trim_end_matches(".lua").to_string(), error),
        };
        result.push(PluginInfo {
            enabled: !disabled.contains(&file.to_ascii_lowercase()),
            file,
            name,
            error,
        });
    }
    result.sort_by_key(|plugin| plugin.file.to_ascii_lowercase());
    Ok(result)
}

#[tauri::command]
pub fn plugins_path(app: AppHandle) -> Result<String, String> {
    Ok(crate::storage::plugins_dir(&app)?
        .to_string_lossy()
        .into_owned())
}

#[tauri::command]
pub fn plugin_add_example(app: AppHandle) -> Result<String, String> {
    let path = crate::storage::plugins_dir(&app)?.join("hello.lua");
    if path.exists() {
        return Err("hello.lua already exists".into());
    }
    fs::write(&path, r#"-- jIRC sandboxed Luau plugin example
return {
  name = "Hello",
  on_event = function(event, payload)
    if event == "command" and payload.command == "hello" then
      return {{ type = "echo", target = payload.target, text = "Hello from a sandboxed plugin!" }}, true
    end
    return {}, false
  end
}
"#).map_err(|error| error.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

#[tauri::command]
pub fn plugin_set_enabled(app: AppHandle, name: String, enabled: bool) -> Result<(), String> {
    let file = safe_file(&name)?;
    let dir = crate::storage::plugins_dir(&app)?;
    if !dir.join(&file).is_file() {
        return Err("plugin file not found".into());
    }
    let mut values = disabled(&dir);
    if enabled {
        values.remove(&file.to_ascii_lowercase());
    } else {
        values.insert(file.to_ascii_lowercase());
    }
    save_disabled(&dir, &values)
}

fn dispatch_from_dir(
    dir: &Path,
    event: &str,
    payload: &serde_json::Value,
) -> Result<PluginDispatch, String> {
    let disabled = disabled(dir);
    let mut result = PluginDispatch::default();
    for entry in fs::read_dir(dir).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let file = entry.file_name().to_string_lossy().into_owned();
        if disabled.contains(&file.to_ascii_lowercase())
            || !file.to_ascii_lowercase().ends_with(".lua")
            || entry.metadata().map_err(|error| error.to_string())?.len() > MAX_SOURCE_BYTES
        {
            continue;
        }
        match fs::read_to_string(entry.path())
            .map_err(|error| error.to_string())
            .and_then(|source| run(&source, event, payload))
        {
            Ok((actions, handled)) => {
                result.actions.extend(actions);
                result.handled |= handled;
            }
            Err(error) => result.errors.push(format!("{file}: {error}")),
        }
        if result.actions.len() >= MAX_ACTIONS {
            result.actions.truncate(MAX_ACTIONS);
            break;
        }
    }
    Ok(result)
}

#[tauri::command]
pub async fn plugin_dispatch(
    app: AppHandle,
    event: String,
    payload: serde_json::Value,
) -> Result<PluginDispatch, String> {
    let dir = crate::storage::plugins_dir(&app)?;
    tauri::async_runtime::spawn_blocking(move || dispatch_from_dir(&dir, &event, &payload))
        .await
        .map_err(|error| error.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandboxed_plugin_receives_events_and_returns_validated_actions() {
        let source = r#"
            return {
                name = "Greeter",
                on_event = function(event, payload)
                    if event == "command" and payload.command == "hello" then
                        return {{ type = "echo", target = payload.target, text = "Hello " .. payload.nick }}, true
                    end
                    return {}, false
                end
            }
        "#;
        let (actions, handled) = run(
            source,
            "command",
            &serde_json::json!({
                "command": "hello", "target": "#jirc", "nick": "John"
            }),
        )
        .unwrap();
        assert!(handled);
        assert_eq!(
            actions,
            vec![PluginAction::Echo {
                target: "#jirc".into(),
                text: "Hello John".into()
            }]
        );
    }

    #[test]
    fn plugin_has_no_os_or_file_library() {
        let source = r#"
            return { name = "Safe", on_event = function()
                return {{ type = "echo", target = "", text = tostring(os) .. ":" .. tostring(io) }}, false
            end }
        "#;
        let (actions, _) = run(source, "start", &serde_json::json!({})).unwrap();
        assert!(matches!(&actions[0], PluginAction::Echo { text, .. } if text == "nil:nil"));
    }

    #[test]
    fn runaway_plugin_is_interrupted() {
        let source = r#"
            return { name = "Loop", on_event = function()
                while true do end
            end }
        "#;
        let started = Instant::now();
        let error = run(source, "start", &serde_json::json!({})).unwrap_err();
        assert!(error.contains("time limit"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
