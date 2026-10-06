//! Read-only recovery of the user's tmux-group-state schema (version 1).
use super::*;
use serde_json::Value;

pub(super) fn read_groups() -> Value {
    let path = std::env::var_os("TMUX_GROUP_STATE_FILE")
        .map(PathBuf::from)
        .or_else(|| {
            let base = std::env::var_os("XDG_STATE_HOME")
                .map(PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
                })?;
            Some(base.join("tmux/window-groups.json"))
        });
    path.and_then(|p| std::fs::read(p).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .filter(|v: &Value| v["version"] == 1 && v["sessions"].is_object())
        .unwrap_or(Value::Null)
}

fn valid(record: &Value) -> bool {
    matches!(
        record["group"].as_str(),
        Some("pinned" | "build" | "explore" | "personal")
    )
}

pub(super) fn apply_groups(
    windows: &mut BTreeMap<(String, String), WindowInfo>,
    keys: &HashMap<(String, String), String>,
    groups: &Value,
) {
    let mut used: HashSet<(String, String)> = keys
        .iter()
        .filter(|(_, key)| !key.is_empty())
        .map(|((session, _), key)| (session.clone(), key.clone()))
        .collect();
    // UUID matches are authoritative, regardless of changed names or paths.
    for (id, window) in windows.iter_mut() {
        if window.group.is_some() {
            continue;
        }
        let key = &keys[id];
        let record = &groups["sessions"][&window.session]["windows"][key];
        if !key.is_empty() && valid(record) {
            window.group = record["group"].as_str().map(str::to_owned);
        }
    }
    let mut ordered: Vec<_> = windows.values_mut().collect();
    ordered.sort_by(|a, b| (&a.session, a.index).cmp(&(&b.session, b.index)));
    for window in ordered {
        if window.group.is_some() {
            continue;
        }
        let Some(records) = groups["sessions"][&window.session]["windows"].as_object() else {
            continue;
        };
        let path = window
            .active_pane()
            .map(|p| p.cwd.to_string_lossy().into_owned())
            .unwrap_or_default();
        let available: Vec<_> = records
            .iter()
            .filter(|(key, value)| {
                valid(value) && !used.contains(&(window.session.clone(), (*key).clone()))
            })
            .collect();
        let mut candidates: Vec<_> = available
            .iter()
            .copied()
            .filter(|(_, record)| {
                record["name"].as_str() == Some(&window.name)
                    && record["path"].as_str() == Some(&path)
                    && record["panes"].as_u64() == Some(window.panes.len() as u64)
            })
            .collect();
        if candidates.is_empty() {
            candidates = available
                .iter()
                .copied()
                .filter(|(_, record)| {
                    record["name"].as_str() == Some(&window.name)
                        && record["path"].as_str() == Some(&path)
                })
                .collect();
        }
        if candidates.is_empty() {
            let paths: Vec<_> = available
                .iter()
                .copied()
                .filter(|(_, record)| record["path"].as_str() == Some(&path))
                .collect();
            if paths.len() == 1 {
                candidates = paths;
            }
        }
        if let Some((key, record)) = candidates
            .into_iter()
            .max_by_key(|(_, r)| r["last_seen"].as_i64().unwrap_or(0))
        {
            used.insert((window.session.clone(), key.clone()));
            window.group = record["group"].as_str().map(str::to_owned);
        }
    }
}
