use anyhow::{bail, Context, Result};
use ncm_model::{Harness, Rgb, SessionRef};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs::{self, File},
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

/// Minimal process metadata from a native process-table snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInfo {
    pub pid: u32,
    pub ppid: u32,
    pub command: String,
}

/// Shared process snapshot, refreshed at most once per two seconds across all panes.
#[derive(Debug, Clone, Default)]
pub struct ProcessTree {
    pub processes: Vec<ProcessInfo>,
}

impl ProcessTree {
    pub fn cached() -> Result<Self> {
        Ok(Self::cached_shared()?.as_ref().clone())
    }

    /// Reuse the same process snapshot across panes without copying every process label.
    pub fn cached_shared() -> Result<Arc<Self>> {
        type Snapshot = Option<(Instant, Arc<ProcessTree>)>;
        static CACHE: OnceLock<Mutex<Snapshot>> = OnceLock::new();
        let mut cache = CACHE
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some((time, tree)) = &*cache {
            if time.elapsed() < Duration::from_secs(2) {
                return Ok(tree.clone());
            }
        }
        let tree = Arc::new(Self {
            processes: super::processes::snapshot()?,
        });
        *cache = Some((Instant::now(), tree.clone()));
        Ok(tree)
    }

    pub fn parse(text: &str) -> Self {
        let processes = text
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                let (pid, rest) = line.split_once(char::is_whitespace)?;
                let (ppid, command) = rest.trim_start().split_once(char::is_whitespace)?;
                Some(ProcessInfo {
                    pid: pid.parse().ok()?,
                    ppid: ppid.parse().ok()?,
                    command: command.trim().to_owned(),
                })
            })
            .collect();
        Self { processes }
    }

    /// Includes the pane root itself, breadth first. Cycles in synthetic/stale snapshots stop.
    pub fn descendants(&self, root: u32) -> Vec<&ProcessInfo> {
        let mut queue = VecDeque::from([root]);
        let mut seen = HashSet::new();
        let mut result = Vec::new();
        while let Some(pid) = queue.pop_front() {
            if !seen.insert(pid) {
                continue;
            }
            if let Some(p) = self.processes.iter().find(|p| p.pid == pid) {
                result.push(p);
            }
            queue.extend(
                self.processes
                    .iter()
                    .filter(|p| p.ppid == pid)
                    .map(|p| p.pid),
            );
        }
        result
    }

    /// Only outer harnesses: tool-launched nested harnesses don't replace the pane's session.
    pub fn harness_processes(&self, root: u32) -> Vec<(&ProcessInfo, Harness)> {
        let descendants = self.descendants(root);
        let mut inside = HashSet::new();
        let mut result = Vec::new();
        for p in descendants {
            if inside.contains(&p.ppid) {
                inside.insert(p.pid);
                continue;
            }
            if let Some(h) = harness_for_command(&p.command) {
                inside.insert(p.pid);
                result.push((p, h));
            }
        }
        result
    }

    pub fn harness(&self, root: u32) -> Option<Harness> {
        let found = self.harness_processes(root);
        (found.len() == 1).then(|| found[0].1.clone())
    }
}

/// Match executable names, never arguments or path substrings (e.g. codex-code-mode-host).
pub fn harness_for_command(command: &str) -> Option<Harness> {
    let path = Path::new(command);
    // Recognize only Claude's native install layout, never a bare version label or
    // an arbitrary path containing "claude" (tool arguments are not ownership).
    let native_claude = path.parent().is_some_and(|p| {
        p.file_name().is_some_and(|s| s == "versions")
            && p.parent()
                .and_then(Path::file_name)
                .is_some_and(|s| s == "claude")
    }) && path.file_name().and_then(|s| s.to_str()).is_some_and(|s| {
        let parts: Vec<_> = s.split('.').collect();
        parts.len() == 3
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    });
    if native_claude {
        return harness_for_command("claude");
    }
    let name = Path::new(command)
        .file_name()?
        .to_str()?
        .trim_start_matches('-')
        .to_ascii_lowercase();
    let name = name.strip_suffix(".exe").unwrap_or(&name);
    let color = match name {
        "claude" | "claude-code" => Rgb::new(217, 119, 87),
        "codex" => Rgb::new(16, 163, 127),
        "kilo" | "opencode" | "gemini" | "aider" | "goose" | "amp" | "droid" => {
            Rgb::new(176, 150, 240)
        }
        _ => return None,
    };
    Some(Harness {
        name: if name == "claude-code" {
            "claude"
        } else {
            name
        }
        .into(),
        color,
    })
}

/// Hook-free, read-only roots. Struct fields retain the original public construction contract.
#[derive(Debug, Clone)]
pub struct SessionDiscovery {
    pub claude_root: PathBuf,
    pub codex_root: PathBuf,
}

impl Default for SessionDiscovery {
    fn default() -> Self {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        Self {
            claude_root: std::env::var_os("NCM_CLAUDE_ROOT")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".claude")),
            codex_root: std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".codex")),
        }
    }
}

impl SessionDiscovery {
    /// tmux's stable pane id disambiguates sessions before the process-tree fallback.
    /// PID ancestry to the pane's tmux server prevents collisions on another socket.
    pub fn discover_pane_with_tree(
        &self,
        pane: &ncm_model::PaneInfo,
        session: &str,
        tree: &ProcessTree,
    ) -> Result<Option<SessionRef>> {
        self.discover_pane_with_context(pane, session, tree)
            .map(|(session, _)| session)
    }

    /// Return startup context with the session, sharing its validated pane mapping.
    pub fn discover_pane_with_context(
        &self,
        pane: &ncm_model::PaneInfo,
        session: &str,
        tree: &ProcessTree,
    ) -> Result<(Option<SessionRef>, Option<ncm_model::Timestamp>)> {
        let mapped = self.mapped_claude_pids(pane, session, tree)?;
        if let [pid] = mapped.as_slice() {
            let session = self.claude_session(*pid, harness_for_command("claude").unwrap())?;
            let started = self
                .claude_metadata(*pid)?
                .as_ref()
                .and_then(metadata_started_at);
            return Ok((session, started));
        }
        if mapped.len() > 1 {
            return Ok((None, None));
        }
        let found = tree.harness_processes(pane.pid);
        if let [(process, harness)] = found.as_slice() {
            if harness.name == "claude" {
                if let Some(metadata) = self.claude_metadata(process.pid)? {
                    // A present mapping is authoritative. Never bypass a wrong session or
                    // pane via the legacy process-tree lookup (including malformed mappings).
                    if metadata.get("tmux").is_some() {
                        return Ok((None, None));
                    }
                }
            }
        }
        let session = self.discover_with_tree(pane.pid, tree)?;
        let started = self.claude_started_at(pane.pid, tree);
        Ok((session, started))
    }

    fn mapped_claude_pids(
        &self,
        pane: &ncm_model::PaneInfo,
        session: &str,
        tree: &ProcessTree,
    ) -> Result<Vec<u32>> {
        let mut matches = Vec::new();
        for entry in read_dir(&self.claude_root.join("sessions"))? {
            let path = entry.path();
            // .key files and non-PID metadata cannot identify a live session.
            if path.extension().is_none_or(|s| s != "json") {
                continue;
            }
            let Some(pid) = path
                .file_stem()
                .and_then(|s| s.to_str())
                .and_then(|s| s.parse::<u32>().ok())
            else {
                continue;
            };
            let Some(metadata) = self.read_claude_metadata(pid).ok().flatten() else {
                continue;
            };
            let Some(mapping) = metadata["tmux"].as_str().and_then(tmux_mapping) else {
                continue;
            };
            if mapping.0 != session || mapping.1 != pane.id {
                continue;
            }
            if !Self::valid_claude_metadata(pid, &metadata) {
                continue;
            }
            // The mapping wins even with sibling/nested harnesses, but must belong to this
            // server. A descendant also proves ownership when the pane root was reparented.
            let descendants = tree.descendants(pane.pid);
            let owns = descendants.iter().any(|p| p.pid == pid);
            let same_server = !owns
                && tree
                    .processes
                    .iter()
                    .find(|p| p.pid == pane.pid)
                    .is_some_and(|root| {
                        tree.processes
                            .iter()
                            .find(|p| p.pid == root.ppid)
                            .is_some_and(|server| {
                                (server.command == "tmux"
                                    || server.command.starts_with("tmux: server"))
                                    && tree.descendants(server.pid).iter().any(|p| p.pid == pid)
                            })
                    });
            if (owns || same_server) && tree.processes.iter().any(|p| p.pid == pid) {
                matches.push(pid);
            }
        }
        Ok(matches)
    }

    pub fn discover(&self, pane_pid: u32) -> Result<Option<SessionRef>> {
        self.discover_with_tree(pane_pid, &ProcessTree::cached()?)
    }

    /// Inject a process snapshot for callers already polling processes or for hand-written tests.
    pub fn discover_with_tree(
        &self,
        pane_pid: u32,
        tree: &ProcessTree,
    ) -> Result<Option<SessionRef>> {
        let found = tree.harness_processes(pane_pid);
        // A supervisor pane with multiple independent agents has no uniquely owned Talk session.
        if found.len() != 1 {
            return Ok(None);
        }
        let (process, harness) = &found[0];
        match harness.name.as_str() {
            "claude" => self.claude_session(process.pid, harness.clone()),
            "codex" => {
                let pids: Vec<_> = tree
                    .descendants(process.pid)
                    .iter()
                    .map(|p| p.pid)
                    .collect();
                let files = owned_transcripts(&pids, &self.codex_root.join("sessions"), true)?;
                // Prefer the process's own file over a child's. Multiple open rollouts in one
                // process (app-server sessions) are ambiguous; never choose by modification time.
                let Some(path) = codex_owned_path(process.pid, &files) else {
                    return Ok(None);
                };
                let id = codex_id(path)?;
                Ok(Some(SessionRef {
                    harness: harness.clone(),
                    session_id: id,
                    transcript_path: path.clone(),
                }))
            }
            _ => Ok(None),
        }
    }

    /// Process startup is distinct from resumed transcript history. Never infer it from mtime.
    pub fn claude_started_at(
        &self,
        pane_pid: u32,
        tree: &ProcessTree,
    ) -> Option<ncm_model::Timestamp> {
        let found = tree.harness_processes(pane_pid);
        let [(process, harness)] = found.as_slice() else {
            return None;
        };
        if harness.name != "claude" {
            return None;
        }
        metadata_started_at(&self.claude_metadata(process.pid).ok()??)
    }

    pub fn claude_pane_started_at(
        &self,
        pane: &ncm_model::PaneInfo,
        session: &str,
        tree: &ProcessTree,
    ) -> Option<ncm_model::Timestamp> {
        let mapped = self.mapped_claude_pids(pane, session, tree).ok()?;
        match mapped.as_slice() {
            [pid] => metadata_started_at(&self.claude_metadata(*pid).ok()??),
            [] => self.claude_started_at(pane.pid, tree),
            _ => None,
        }
    }

    /// Explicit session lookup for the diagnostic example; never guesses the newest project file.
    pub fn session(&self, id: &str) -> Result<Option<SessionRef>> {
        if !safe_id(id) {
            bail!("session id must be a file basename");
        }
        let matches = claude_paths(&self.claude_root, id)?;
        if matches.len() == 1 {
            return Ok(Some(SessionRef {
                harness: harness_for_command("claude").unwrap(),
                session_id: id.into(),
                transcript_path: matches[0].clone(),
            }));
        }
        let mut dirs = vec![self.codex_root.join("sessions")];
        let mut matches = Vec::new();
        while let Some(dir) = dirs.pop() {
            for entry in read_dir(&dir)? {
                if entry.file_type()?.is_dir() {
                    dirs.push(entry.path());
                } else if entry
                    .file_name()
                    .to_string_lossy()
                    .ends_with(&format!("-{id}.jsonl"))
                {
                    matches.push(entry.path());
                }
            }
        }
        if matches.len() == 1 {
            return Ok(Some(SessionRef {
                harness: harness_for_command("codex").unwrap(),
                session_id: id.into(),
                transcript_path: matches.remove(0),
            }));
        }
        Ok(None)
    }

    fn read_claude_metadata(&self, pid: u32) -> Result<Option<Value>> {
        let path = self
            .claude_root
            .join("sessions")
            .join(format!("{pid}.json"));
        let metadata: Value = match File::open(&path) {
            Ok(file) => serde_json::from_reader(BufReader::new(file))
                .context("parse Claude live session metadata")?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        Ok(Some(metadata))
    }

    fn valid_claude_metadata(pid: u32, metadata: &Value) -> bool {
        if metadata["pid"].as_u64() != Some(pid as u64) {
            return false;
        }
        let Some((live_start, labels)) = super::processes::start(pid) else {
            return false;
        };
        if metadata.get("procStart").is_some_and(|v| {
            !labels
                .iter()
                .any(|label| v.as_str() == Some(label.as_str()))
        }) {
            return false;
        }
        // startedAt is written after CLI startup, not the exact fork instant. Allow up to
        // ten seconds of initialization; procStart (when present) must match to the second.
        if metadata.get("startedAt").is_some()
            && metadata_started_at(metadata).is_none_or(|t| {
                !(live_start.0..=live_start.0.saturating_add(10_000)).contains(&t.0)
            })
        {
            return false;
        }
        true
    }

    fn claude_metadata(&self, pid: u32) -> Result<Option<Value>> {
        Ok(self
            .read_claude_metadata(pid)?
            .filter(|metadata| Self::valid_claude_metadata(pid, metadata)))
    }

    fn claude_session(&self, pid: u32, harness: Harness) -> Result<Option<SessionRef>> {
        let Some(metadata) = self.claude_metadata(pid)? else {
            return Ok(None);
        };
        let Some(id) = metadata
            .get("sessionId")
            .and_then(Value::as_str)
            .filter(|id| safe_id(id))
        else {
            return Ok(None);
        };
        let paths = claude_paths(&self.claude_root, id)?;
        // Only direct kernel ownership can override stale metadata after a session switch.
        // lsof isn't run for Claude on every refresh: libproc is cheap; mtime is not ownership.
        let owned: Vec<_> = native_open_files(pid)
            .unwrap_or_default()
            .into_iter()
            .filter(|p| {
                p.starts_with(self.claude_root.join("projects"))
                    && p.extension().is_some_and(|x| x == "jsonl")
                    && !p.components().any(|c| c.as_os_str() == "subagents")
            })
            .collect();
        let transcript_path = if owned.len() == 1 {
            owned[0].clone()
        } else if paths.len() == 1 {
            paths[0].clone()
        } else {
            return Ok(None);
        };
        let session_id = transcript_path
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        Ok(Some(SessionRef {
            harness,
            session_id,
            transcript_path,
        }))
    }
}

fn metadata_started_at(metadata: &Value) -> Option<ncm_model::Timestamp> {
    metadata["startedAt"]
        .as_i64()
        .map(ncm_model::Timestamp)
        .or_else(|| {
            metadata["startedAt"]
                .as_str()
                .and_then(crate::parse_timestamp)
        })
}

fn tmux_mapping(text: &str) -> Option<(&str, &str)> {
    let (session, ids) = text.rsplit_once(":@")?;
    let (window, pane) = ids.split_once('.')?;
    let digits = pane.strip_prefix('%')?;
    (!session.is_empty()
        && !window.is_empty()
        && window.bytes().all(|b| b.is_ascii_digit())
        && !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit()))
    .then_some((session, pane))
}

fn codex_owned_path(root_pid: u32, files: &[(u32, PathBuf)]) -> Option<&PathBuf> {
    let own: HashSet<_> = files
        .iter()
        .filter(|(pid, _)| *pid == root_pid)
        .map(|(_, path)| path)
        .collect();
    if own.len() == 1 {
        return own.into_iter().next();
    }
    if own.len() > 1 {
        return None;
    }
    // Several children can hold the same file, but distinct child sessions are ambiguous.
    let children: HashSet<_> = files.iter().map(|(_, path)| path).collect();
    if children.len() == 1 {
        children.into_iter().next()
    } else {
        None
    }
}

fn safe_id(id: &str) -> bool {
    !id.is_empty() && id != "." && id != ".." && !id.contains(['/', '\\'])
}

fn read_dir(path: &Path) -> Result<Vec<fs::DirEntry>> {
    match fs::read_dir(path) {
        Ok(entries) => entries
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(Into::into),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}

fn claude_paths(root: &Path, id: &str) -> Result<Vec<PathBuf>> {
    Ok(read_dir(&root.join("projects"))?
        .into_iter()
        .map(|entry| entry.path().join(format!("{id}.jsonl")))
        .filter(|p| p.is_file())
        .collect())
}

fn codex_id(path: &Path) -> Result<String> {
    let mut line = String::new();
    BufReader::new(File::open(path)?)
        .take(1024 * 1024)
        .read_line(&mut line)?;
    if let Ok(record) = serde_json::from_str::<Value>(&line) {
        if record["type"] == "session_meta" {
            if let Some(id) = record["payload"]["id"]
                .as_str()
                .or_else(|| record["payload"]["session_id"].as_str())
            {
                return Ok(id.to_owned());
            }
        }
    }
    // Filename ends in the UUID, independent of the date/time punctuation.
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    Ok(if stem.len() >= 36 {
        stem[stem.len() - 36..].to_owned()
    } else {
        stem.into_owned()
    })
}

fn owned_transcripts(pids: &[u32], root: &Path, rollout: bool) -> Result<Vec<(u32, PathBuf)>> {
    // A negative lookup is cached too: do not repeatedly enumerate a process's fds
    // when it has no rollout. A new process identity or disappeared file invalidates it.
    type OpenCache = HashMap<(u32, PathBuf, bool), (u64, Vec<PathBuf>)>;
    static CACHE: OnceLock<Mutex<OpenCache>> = OnceLock::new();
    let mut cache = CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let matches = |p: &Path| {
        (p.starts_with(root) || (rollout && p.components().any(|c| c.as_os_str() == "sessions")))
            && p.extension().is_some_and(|e| e == "jsonl")
            && (!rollout
                || p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("rollout-")))
    };
    let mut files = Vec::new();
    for &pid in pids {
        let key = (pid, root.to_owned(), rollout);
        let Some(identity) = super::processes::identity(pid) else {
            cache.remove(&key);
            continue;
        };
        if cache
            .get(&key)
            .is_none_or(|(old, paths)| *old != identity || paths.iter().any(|p| !p.is_file()))
        {
            // Errors can be temporary (permissions/process exit), so do not cache failures.
            if let Ok(paths) = native_open_files(pid) {
                cache.insert(
                    key.clone(),
                    (identity, paths.into_iter().filter(|p| matches(p)).collect()),
                );
            }
        }
        if let Some((_, paths)) = cache.get(&key) {
            files.extend(
                paths
                    .iter()
                    .filter(|p| matches(p))
                    .cloned()
                    .map(|p| (pid, p)),
            );
        }
    }
    // Bound dead-process entries without enumerating their descriptors.
    if cache.len() > 256 {
        cache.retain(|(pid, _, _), _| super::processes::identity(*pid).is_some());
    }
    files.sort();
    files.dedup();
    Ok(files)
}

#[cfg(target_os = "macos")]
fn native_open_files(pid: u32) -> std::io::Result<Vec<PathBuf>> {
    use std::{
        ffi::CStr,
        mem::{size_of, MaybeUninit},
    };
    // sys/proc_info.h; libc does not expose this flavor constant.
    const PROC_PIDFDVNODEPATHINFO: i32 = 2;
    #[repr(C)]
    struct FileInfo {
        flags: u32,
        status: u32,
        offset: i64,
        kind: i32,
        guards: u32,
    }
    #[repr(C)]
    struct VnodePath {
        file: FileInfo,
        vnode: libc::vnode_info_path,
    }
    // SAFETY: libproc's size query has no output buffer; subsequent calls receive properly
    // aligned C-layout buffers with their exact lengths. Returned byte lengths are checked.
    let needed = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDLISTFDS,
            0,
            std::ptr::null_mut(),
            0,
        )
    };
    if needed <= 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut fds = vec![
        libc::proc_fdinfo {
            proc_fd: 0,
            proc_fdtype: 0
        };
        needed as usize / size_of::<libc::proc_fdinfo>() + 32
    ];
    let bytes = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDLISTFDS,
            0,
            fds.as_mut_ptr().cast(),
            std::mem::size_of_val(fds.as_slice()) as i32,
        )
    };
    if bytes <= 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut paths = Vec::new();
    for fd in fds
        .iter()
        .take(bytes as usize / size_of::<libc::proc_fdinfo>())
    {
        if fd.proc_fdtype != libc::PROX_FDTYPE_VNODE as u32 {
            continue;
        }
        let mut data = MaybeUninit::<VnodePath>::zeroed();
        let count = unsafe {
            libc::proc_pidfdinfo(
                pid as i32,
                fd.proc_fd,
                PROC_PIDFDVNODEPATHINFO,
                data.as_mut_ptr().cast(),
                size_of::<VnodePath>() as i32,
            )
        };
        if count != size_of::<VnodePath>() as i32 {
            continue;
        }
        let data = unsafe { data.assume_init() };
        if !data.vnode.vip_path.iter().flatten().any(|c| *c == 0) {
            continue;
        }
        let name = unsafe { CStr::from_ptr(data.vnode.vip_path.as_ptr().cast()) };
        use std::os::unix::ffi::OsStrExt;
        paths.push(PathBuf::from(std::ffi::OsStr::from_bytes(name.to_bytes())));
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

#[cfg(target_os = "linux")]
fn native_open_files(pid: u32) -> std::io::Result<Vec<PathBuf>> {
    Ok(fs::read_dir(format!("/proc/{pid}/fd"))?
        .filter_map(|entry| fs::read_link(entry.ok()?.path()).ok())
        .collect())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn native_open_files(_pid: u32) -> std::io::Result<Vec<PathBuf>> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "libproc unavailable",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane() -> ncm_model::PaneInfo {
        ncm_model::PaneInfo {
            id: "%40".into(),
            index: 0,
            x: 0,
            y: 0,
            width: 80,
            height: 24,
            active: true,
            pid: 1,
            cwd: PathBuf::new(),
            title: String::new(),
            cursor: Default::default(),
            tint: None,
            harness: None,
        }
    }

    #[test]
    fn synthetic_discovery_attaches_adapter_with_mapping_and_legacy_tree() -> Result<()> {
        let pid = std::process::id();
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../../target/discovery-e2e-{pid}"));
        let discovery = SessionDiscovery {
            claude_root: dir.join("claude"),
            codex_root: dir.join("codex"),
        };
        fs::create_dir_all(discovery.claude_root.join("sessions"))?;
        fs::create_dir_all(discovery.claude_root.join("projects/project"))?;
        let metadata_path = discovery.claude_root.join(format!("sessions/{pid}.json"));
        let transcript = discovery.claude_root.join("projects/project/owned.jsonl");
        fs::write(&transcript, concat!(
            "{\"type\":\"user\",\"uuid\":\"u\",\"origin\":{\"kind\":\"human\"},\"message\":{\"content\":\"ask\"}}\n",
            "{\"type\":\"assistant\",\"parentUuid\":\"u\",\"message\":{\"model\":\"test-model\",\"usage\":{\"input_tokens\":123},\"content\":[{\"type\":\"text\",\"text\":\"reply\"}]}}\n"
        ))?;
        fs::write(
            discovery
                .claude_root
                .join(format!("sessions/{pid}.hash.key")),
            "not JSON",
        )?;
        let (started, proc_start) = super::super::processes::start(pid).unwrap();
        let base = serde_json::json!({"pid":pid, "sessionId":"owned", "startedAt":started.0, "procStart":proc_start[0]});
        for (mapping, command, extra) in [
            (None, "/Users/test/.local/share/claude/versions/2.1.289", ""),
            (None, "claude", ""),
            (Some("main:@6.%40"), "2.1.289", "900001 1 codex\n"),
        ] {
            let mut metadata = base.clone();
            if let Some(mapping) = mapping {
                metadata["tmux"] = mapping.into();
            }
            fs::write(&metadata_path, metadata.to_string())?;
            let tree =
                ProcessTree::parse(&format!("10 0 tmux\n1 10 zsh\n{pid} 1 {command}\n{extra}"));
            let session = discovery
                .discover_pane_with_tree(&pane(), "main", &tree)?
                .unwrap();
            assert_eq!(session.session_id, "owned");
            assert_eq!(session.transcript_path, transcript);
            let mut adapter = crate::adapter_for(&session).expect("discovered session must attach");
            let mut follower = crate::TranscriptFollower::new(&session);
            let events = follower.poll_adapter(&mut *adapter)?.events;
            assert_eq!(
                crate::TalkView::from_events(&events, crate::now())
                    .blocks
                    .len(),
                2
            );
            assert_eq!(adapter.facts().model.as_deref(), Some("test-model"));
            assert_eq!(adapter.facts().context_tokens, Some(123));
        }
        // Hooks can write UTC lstart labels while ncm uses the machine's local timezone.
        let mut utc_metadata = base.clone();
        utc_metadata["procStart"] = proc_start[1].clone().into();
        fs::write(&metadata_path, utc_metadata.to_string())?;
        let utc_tree = ProcessTree::parse(&format!("1 0 zsh\n{pid} 1 claude\n"));
        assert!(discovery
            .discover_pane_with_tree(&pane(), "main", &utc_tree)?
            .is_some());
        let mut mapped = base.clone();
        mapped["tmux"] = "main:@999.%40".into(); // Window id is not the stable pane identity.
        fs::write(&metadata_path, mapped.to_string())?;
        let same_server =
            ProcessTree::parse(&format!("10 0 tmux\n1 10 zsh\n2 10 zsh\n{pid} 2 2.1.289\n"));
        assert!(discovery
            .discover_pane_with_tree(&pane(), "main", &same_server)?
            .is_some());
        let other_server = ProcessTree::parse(&format!(
            "10 0 tmux\n1 10 zsh\n20 0 tmux\n2 20 zsh\n{pid} 2 claude\n"
        ));
        assert!(discovery
            .discover_pane_with_tree(&pane(), "main", &other_server)?
            .is_none());
        let tree = ProcessTree::parse(&format!("1 0 zsh\n{pid} 1 claude\n"));
        for mapping in ["other:@6.%40", "main:@6.%41", "malformed"] {
            mapped["tmux"] = mapping.into();
            fs::write(&metadata_path, mapped.to_string())?;
            assert!(discovery
                .discover_pane_with_tree(&pane(), "main", &tree)?
                .is_none());
        }
        for (field, value) in [
            ("pid", serde_json::json!(pid + 1)),
            ("procStart", serde_json::json!("Sat Oct  3 00:00:00 2020")),
            ("startedAt", serde_json::json!(started.0 - 60_000)),
            ("startedAt", serde_json::json!(started.0 + 60_000)),
        ] {
            let mut stale = base.clone();
            stale[field] = value;
            fs::write(&metadata_path, stale.to_string())?;
            assert!(discovery
                .discover_pane_with_tree(&pane(), "main", &tree)?
                .is_none());
        }
        let dead = 2_000_000_000;
        fs::write(
            discovery.claude_root.join(format!("sessions/{dead}.json")),
            serde_json::json!({"pid":dead, "sessionId":"owned", "tmux":"main:@6.%40"}).to_string(),
        )?;
        let tree = ProcessTree::parse(&format!("1 0 zsh\n{dead} 1 claude\n"));
        assert!(discovery
            .discover_pane_with_tree(&pane(), "main", &tree)?
            .is_none());
        assert!(harness_for_command("2.1.289").is_none());
        assert!(harness_for_command("/tmp/claude/2.1.289").is_none());
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn native_rollout_lookup_is_cached_until_the_file_disappears() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("ncm-fds-{}", std::process::id()));
        fs::create_dir_all(&dir)?;
        let dir = dir.canonicalize()?;
        let first = dir.join("rollout-first.jsonl");
        let second = dir.join("rollout-second.jsonl");
        let open_first = File::create(&first)?;
        let pid = std::process::id();
        assert_eq!(
            owned_transcripts(&[pid], &dir, true)?,
            [(pid, first.clone())]
        );
        let open_second = File::create(&second)?;
        // Opening another file does not cause descriptor enumeration on every lookup.
        assert_eq!(
            owned_transcripts(&[pid], &dir, true)?,
            [(pid, first.clone())]
        );
        drop(open_first);
        fs::remove_file(&first)?;
        assert_eq!(
            owned_transcripts(&[pid], &dir, true)?,
            [(pid, second.clone())]
        );
        drop(open_second);
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn codex_child_sessions_must_be_unique_without_a_root_owned_file() {
        let a = PathBuf::from("a.jsonl");
        let b = PathBuf::from("b.jsonl");
        assert!(codex_owned_path(1, &[(2, a.clone()), (3, b.clone())]).is_none());
        assert_eq!(
            codex_owned_path(1, &[(2, a.clone()), (3, a.clone())]),
            Some(&a)
        );
        assert_eq!(
            codex_owned_path(1, &[(1, a.clone()), (2, b.clone())]),
            Some(&a)
        );
        assert!(codex_owned_path(1, &[(1, a), (1, b)]).is_none());
    }
}
