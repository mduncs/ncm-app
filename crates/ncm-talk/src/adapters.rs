use crate::{now, parse_timestamp, TranscriptAdapter};
use anyhow::Result;
use ncm_model::{
    Facts, NowKind, NowLine, SessionRef, Subagent, SubagentState, TalkEvent, Timestamp,
};
use serde_json::Value;
use std::{
    borrow::Cow,
    cell::RefCell,
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    time::{Duration, Instant, UNIX_EPOCH},
};

pub fn adapter_for(session: &SessionRef) -> Option<Box<dyn TranscriptAdapter>> {
    match session.harness.name.as_str() {
        "claude" => Some(Box::<ClaudeAdapter>::default()),
        "codex" => Some(Box::<CodexAdapter>::default()),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Origin {
    Human,
    Report(Option<String>),
}

#[derive(Debug, Clone)]
struct Node {
    parent: Option<String>,
    origin: Option<Origin>,
    turn: Option<String>,
}

#[derive(Debug, Clone)]
struct Tool {
    name: String,
    summary: String,
    order: u64,
}

#[derive(Debug, Clone)]
struct Agent {
    name: String,
    state: SubagentState,
    started: Option<Timestamp>,
    finished: bool,
    launched: bool,
}

#[derive(Debug, Default)]
struct Activity {
    model: Option<String>,
    context: Option<u64>,
    latest_text: String,
    pending: HashMap<String, Tool>,
    agents: HashMap<String, Agent>,
    aliases: HashMap<String, String>,
    order: u64,
}

impl Activity {
    fn tool(&mut self, id: &str, name: &str, args: &Value) {
        self.order += 1;
        self.pending.insert(
            id.to_owned(),
            Tool {
                name: name.into(),
                summary: args_summary(args),
                order: self.order,
            },
        );
    }
    fn finish_tool(&mut self, id: &str) {
        self.pending.remove(id);
    }
    fn facts_at(&self, time: Timestamp) -> Facts {
        let now = match self.pending.values().max_by_key(|tool| tool.order) {
            Some(tool) => NowLine {
                kind: NowKind::Tool,
                text: format!("{}({})", tool.name, tool.summary),
                tool: Some(tool.name.clone()),
            },
            None => NowLine {
                kind: NowKind::Text,
                text: self.latest_text.clone(),
                tool: None,
            },
        };
        let mut subagents: Vec<_> = self
            .agents
            .values()
            .map(|agent| Subagent {
                name: agent.name.clone(),
                state: agent.state,
                running_time: if agent.state == SubagentState::Busy {
                    agent.started.map(|start| start.age_at(time))
                } else {
                    None
                },
            })
            .collect();
        subagents.sort_by(|a, b| a.name.cmp(&b.name));
        Facts {
            now,
            model: self.model.clone(),
            context_tokens: self.context,
            subagents,
            hidden_count: 0,
        }
    }
    fn spawn(&mut self, id: &str, input: &Value, timestamp: Option<Timestamp>) {
        let name = ["name", "description", "subagent_type"]
            .into_iter()
            .find_map(|key| input[key].as_str())
            .unwrap_or(id);
        self.agents.entry(id.into()).or_insert_with(|| Agent {
            name: name.into(),
            state: SubagentState::Busy,
            started: timestamp,
            finished: false,
            launched: true,
        });
        self.aliases.insert(name.into(), id.into());
    }
    fn agent_result(&mut self, tool_id: &str, result: &Value) {
        if !self.agents.contains_key(tool_id) {
            return;
        }
        if let Some(id) = result["agentId"]
            .as_str()
            .or_else(|| result["agent_id"].as_str())
            .or_else(|| result["taskId"].as_str())
        {
            self.aliases.insert(id.into(), tool_id.into());
        }
        if let Some(name) = result["name"]
            .as_str()
            .or_else(|| result["nickname"].as_str())
        {
            self.agents.get_mut(tool_id).unwrap().name = name.into();
            self.aliases.insert(name.into(), tool_id.into());
        }
        // Background launch tool completion is not agent completion.
        if matches!(
            result["status"].as_str(),
            Some("completed" | "failed" | "killed")
        ) {
            let agent = self.agents.get_mut(tool_id).unwrap();
            agent.state = SubagentState::Idle;
            agent.finished = true;
        }
    }
    fn agent_statuses(&mut self, result: &Value) {
        if let Some(statuses) = result["status"].as_object() {
            for (id, status) in statuses {
                let finished = status.get("completed").is_some()
                    || status.get("errored").is_some()
                    || status == "completed"
                    || status == "shutdown";
                if finished {
                    if let Some(agent) = self
                        .aliases
                        .get(id)
                        .and_then(|key| self.agents.get_mut(key))
                    {
                        agent.state = SubagentState::Idle;
                        agent.finished = true;
                    }
                }
            }
        }
    }
    fn notification(&mut self, name: &str, body: &str, timestamp: Option<Timestamp>, peer: bool) {
        let tool_id = tag(body, "tool-use-id");
        let task_id = tag(body, "task-id");
        let key = tool_id
            .filter(|id| self.agents.contains_key(*id))
            .map(str::to_owned)
            .or_else(|| task_id.and_then(|id| self.aliases.get(id).cloned()))
            .or_else(|| self.aliases.get(name).cloned())
            .unwrap_or_else(|| name.to_owned());
        // Task notifications also describe background shell commands. Those are reports, but
        // they aren't subagents unless a spawn/alias or an Agent summary proves identity.
        if !peer && !self.agents.contains_key(&key) && summary_agent(body).is_none() {
            return;
        }
        let status = tag(body, "status");
        let peer_body = peer.then(|| peer_payload(body).1.to_owned());
        let peer_json = peer_body
            .as_deref()
            .and_then(|text| serde_json::from_str::<Value>(text).ok());
        let finished = matches!(
            status,
            Some("completed" | "failed" | "killed" | "stopped" | "finished")
        ) || tag(body, "summary").is_some_and(|s| s.contains(" finished"))
            || tag(body, "event") == Some("idle")
            || peer_json.as_ref().is_some_and(|message| {
                message["type"] == "idle_notification" || message["type"] == "shutdown_response"
            });
        let agent = self.agents.entry(key.clone()).or_insert_with(|| Agent {
            name: name.into(),
            state: if finished {
                SubagentState::Idle
            } else {
                SubagentState::Busy
            },
            started: if finished { None } else { timestamp },
            finished,
            launched: false,
        });
        agent.name = name.into();
        if finished {
            agent.state = SubagentState::Idle;
            agent.finished = true;
        } else if peer && !agent.finished {
            if agent.state == SubagentState::Idle {
                agent.started = timestamp;
            }
            agent.state = SubagentState::Busy;
            if agent.started.is_none() {
                agent.started = timestamp;
            }
        }
        self.aliases.insert(name.into(), key);
    }
}

/// Claude keeps UUID ancestry and facts, never tool output, thinking, or raw transcript records.
#[derive(Debug, Default)]
pub struct ClaudeAdapter {
    ancestry: HashMap<String, Node>,
    activity: Activity,
    queued_origin: Option<Origin>,
    queued_turn: Option<String>,
    current_turn: Option<String>,
    queued_sources: HashSet<String>,
    prompt_ids: HashSet<String>,
    queued_fingerprints: VecDeque<(usize, u64)>,
    node_order: VecDeque<String>,
    live: Option<(PathBuf, Option<Timestamp>)>,
    freshness: RefCell<HashMap<PathBuf, (Instant, Option<Timestamp>)>>,
}

impl ClaudeAdapter {
    pub fn new() -> Self {
        Self::default()
    }
    // Keep only bounded fingerprints, never queued bodies. Match merged queued deliveries
    // as sequences of queued prompts; ordinary repeated typed prompts remain separate messages.
    fn remember_queued(&mut self, body: &str) {
        self.queued_fingerprints.push_back(prompt_fingerprint(body));
        while self.queued_fingerprints.len() > 2048 {
            self.queued_fingerprints.pop_front();
        }
    }
    fn matches_queued(&self, body: &str) -> bool {
        if self.queued_fingerprints.contains(&prompt_fingerprint(body)) {
            return true;
        }
        let words: Vec<_> = body.split_whitespace().collect();
        if words.is_empty() {
            return false;
        }
        let known: HashSet<_> = self.queued_fingerprints.iter().copied().collect();
        let lengths: HashSet<_> = known
            .iter()
            .map(|(length, _)| *length)
            .filter(|length| *length > 0)
            .collect();
        let mut matched = vec![false; words.len() + 1];
        matched[0] = true;
        for start in 0..words.len() {
            if !matched[start] {
                continue;
            }
            for &length in &lengths {
                let end = start + length;
                if end <= words.len() && known.contains(&words_fingerprint(&words[start..end])) {
                    matched[end] = true;
                }
            }
        }
        matched[words.len()]
    }
    fn modified_at(&self, path: PathBuf) -> Option<Timestamp> {
        let mut cache = self.freshness.borrow_mut();
        if let Some((checked, modified)) = cache.get(&path) {
            if checked.elapsed() < Duration::from_secs(5) {
                return *modified;
            }
        }
        let modified = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|t| Timestamp(t.as_millis().min(i64::MAX as u128) as i64));
        if cache.len() >= 512 {
            cache.retain(|_, (checked, _)| checked.elapsed() < Duration::from_secs(5));
        }
        cache.insert(path, (Instant::now(), modified));
        modified
    }
    pub fn facts_at(&self, time: Timestamp) -> Facts {
        let mut facts = self.activity.facts_at(time);
        if let Some((directory, started)) = &self.live {
            facts.subagents = self.activity.agents.iter().filter_map(|(key, agent)| {
                if matches!((agent.started, started), (Some(launch), Some(start)) if launch < *start) {
                    return None;
                }
                let launched_this_process = agent.launched
                    && matches!((agent.started, started), (Some(launch), Some(start)) if launch >= *start);
                let recent = launched_this_process && !agent.finished
                    && self.activity.aliases.iter()
                        .filter(|(_, target)| *target == key)
                        .map(|(alias, _)| alias.as_str())
                        .chain(std::iter::once(key.as_str()))
                        .filter(|id| !id.contains(['/', '\\']) && !id.starts_with('.'))
                        .any(|id| {
                            self.modified_at(directory.join(format!("agent-{id}.jsonl")))
                                .is_some_and(|modified| modified.age_at(time) < Duration::from_secs(180))
                        });
                let busy = recent && agent.state == SubagentState::Busy;
                Some(Subagent {
                    name: agent.name.clone(),
                    state: if busy { SubagentState::Busy } else { SubagentState::Idle },
                    running_time: if busy { agent.started.map(|start| start.age_at(time)) } else { None },
                })
            }).collect();
            facts.subagents.sort_by(|a, b| a.name.cmp(&b.name));
        }
        facts
    }

    fn before_start(&self, timestamp: Option<Timestamp>) -> bool {
        matches!((timestamp, self.live.as_ref().and_then(|(_, started)| *started)), (Some(time), Some(start)) if time < start)
    }

    fn parent_origin(&self, record: &Value) -> Option<Origin> {
        let mut parent = record["parentUuid"].as_str();
        let mut seen = HashSet::new();
        while let Some(id) = parent {
            if !seen.insert(id) {
                break;
            }
            let node = self.ancestry.get(id)?;
            if let Some(origin) = &node.origin {
                return Some(origin.clone());
            }
            parent = node.parent.as_deref();
        }
        None
    }

    fn report(
        &mut self,
        body: &str,
        origin_kind: Option<&str>,
        from: Option<&str>,
        timestamp: Option<Timestamp>,
    ) -> (Origin, TalkEvent) {
        // Decode fully escaped envelopes only. A literal envelope can contain JSON;
        // decoding quotes inside that JSON before parsing would corrupt its strings.
        let body = if REPORT_MARKERS.iter().any(|marker| body.contains(marker)) {
            Cow::Borrowed(body)
        } else {
            Cow::Owned(unescape_entities(body))
        };
        let peer = [
            "<teammate-message",
            "<cross-session-message",
            "<agent-message",
        ]
        .iter()
        .any(|marker| body.contains(marker))
            || (!body.contains("<task-notification")
                && !matches!(origin_kind, Some("task-notification" | "task_notification")));
        let (agent, report_body) = if peer {
            peer_report(&body, from)
        } else {
            task_report(&body, from)
        };
        if !self.before_start(timestamp) {
            self.activity.notification(&agent, &body, timestamp, peer);
        }
        (
            Origin::Report(Some(agent.clone())),
            TalkEvent::Report {
                timestamp,
                agent,
                body: report_body,
            },
        )
    }

    fn consume_inner(&mut self, r: &Value) -> Vec<TalkEvent> {
        if r["isSidechain"].as_bool() == Some(true) {
            return Vec::new();
        }
        let timestamp = record_timestamp(r);
        let body = message_text(&r["message"]["content"]);
        let kind = r["type"].as_str().unwrap_or("");
        let explicit = origin_kind(&r["turnOrigin"]);
        let parent = self.parent_origin(r);
        let parent_turn = r["parentUuid"]
            .as_str()
            .and_then(|id| self.ancestry.get(id))
            .and_then(|node| node.turn.clone());
        let mut origin = None;
        let mut queued_user = false;
        let mut events = Vec::new();
        match kind {
            "attachment" if r["attachment"]["type"] == "queued_command" => {
                let a = &r["attachment"];
                let prompt = message_text(&a["prompt"]);
                if non_human_prompt(a, &prompt) || a["commandMode"] == "slash" {
                    return events;
                }
                let sources: Vec<_> = [a["source_uuid"].as_str(), a["delivery_id"].as_str()]
                    .into_iter()
                    .flatten()
                    .collect();
                let duplicate = sources
                    .iter()
                    .any(|id| self.prompt_ids.contains(*id) || self.queued_sources.contains(*id));
                for id in sources {
                    self.queued_sources.insert(id.into());
                }
                if !duplicate {
                    self.remember_queued(&prompt);
                }
                let attachment_origin = a["origin"]["kind"].as_str();
                let queued_origin = if content_report(&prompt) || report_origin(attachment_origin) {
                    let (report_origin, event) = self.report(
                        &prompt,
                        attachment_origin,
                        a["origin"]["from"].as_str(),
                        record_timestamp(a).or(timestamp),
                    );
                    if !duplicate && !event.body().is_empty() {
                        events.push(event);
                    }
                    report_origin
                } else {
                    if !prompt.trim().is_empty() && !duplicate {
                        events.push(TalkEvent::You {
                            timestamp: record_timestamp(a).or(timestamp),
                            body: prompt.trim().into(),
                        });
                    }
                    Origin::Human
                };
                if !prompt.trim().is_empty() && !duplicate {
                    self.queued_origin = Some(queued_origin.clone());
                    self.queued_turn = parent_turn.clone().or_else(|| self.current_turn.clone());
                }
                origin = Some(queued_origin);
            }
            "user" => {
                let origin_name = r["origin"]["kind"].as_str();
                let from_queue = r["uuid"]
                    .as_str()
                    .is_some_and(|id| self.queued_sources.contains(id));
                queued_user = from_queue || r["promptSource"] == "queued";
                let matched_queue = queued_user && self.matches_queued(&body);
                if queued_user && !matched_queue {
                    self.remember_queued(&body);
                }
                if content_report(&body) || report_origin(origin_name) {
                    let (report_origin, event) =
                        self.report(&body, origin_name, r["origin"]["from"].as_str(), timestamp);
                    origin = Some(report_origin.clone());
                    self.queued_origin = queued_user.then_some(report_origin);
                    if queued_user {
                        self.queued_turn =
                            parent_turn.clone().or_else(|| self.current_turn.clone());
                    }
                    let duplicate = r["uuid"]
                        .as_str()
                        .is_some_and(|id| !self.prompt_ids.insert(id.into()))
                        || from_queue
                        || matched_queue;
                    if !duplicate && !event.body().is_empty() {
                        events.push(event);
                    }
                } else if !body.trim().is_empty()
                    && !non_human_prompt(r, &body)
                    && (r["isMeta"].as_bool() != Some(true) || queued_user)
                    && (origin_name == Some("human") || origin_name.is_none())
                {
                    origin = Some(Origin::Human);
                    self.queued_origin = None;
                    // Some versions also persist a user record with queued_command.source_uuid.
                    let duplicate = r["uuid"]
                        .as_str()
                        .is_some_and(|id| !self.prompt_ids.insert(id.into()))
                        || from_queue
                        || matched_queue;
                    if queued_user {
                        self.queued_origin = Some(Origin::Human);
                        self.queued_turn =
                            parent_turn.clone().or_else(|| self.current_turn.clone());
                    }
                    if !duplicate {
                        events.push(TalkEvent::You {
                            timestamp,
                            body: body.trim().into(),
                        });
                    }
                }
                if let Some(blocks) = r["message"]["content"].as_array() {
                    for block in blocks {
                        if block["type"] == "tool_result" {
                            let id = block["tool_use_id"].as_str().unwrap_or("");
                            self.activity.finish_tool(id);
                            self.activity.agent_result(id, &r["toolUseResult"]);
                            self.activity.agent_result(id, &block["content"]);
                            // Older async result strings carry only an agentId: line.
                            if let Some(text) = block["content"].as_str() {
                                if let Some(id_value) = text
                                    .split("agentId:")
                                    .nth(1)
                                    .and_then(|s| s.split_whitespace().next())
                                {
                                    self.activity.aliases.insert(id_value.into(), id.into());
                                }
                            }
                        }
                    }
                }
            }
            "assistant" => {
                if let Some(model) = r["message"]["model"]
                    .as_str()
                    .filter(|m| !m.starts_with('<'))
                {
                    self.activity.model = Some(model.into());
                }
                let usage = &r["message"]["usage"];
                if usage.is_object() {
                    self.activity.context = Some(
                        [
                            "input_tokens",
                            "cache_read_input_tokens",
                            "cache_creation_input_tokens",
                        ]
                        .iter()
                        .map(|key| usage[key].as_u64().unwrap_or(0))
                        .sum(),
                    );
                }
                let queued_in_turn = self.queued_origin.is_some()
                    && (parent_turn.is_none() || parent_turn == self.queued_turn);
                origin = if queued_in_turn {
                    self.queued_origin.clone()
                } else if matches!(parent, Some(Origin::Report(_))) {
                    parent.clone()
                } else {
                    explicit
                        .clone()
                        .map(|o| match o {
                            Origin::Report(_) => match &parent {
                                Some(Origin::Report(agent)) => Origin::Report(agent.clone()),
                                _ => o,
                            },
                            _ => o,
                        })
                        .or(parent.clone())
                };
                if !body.trim().is_empty() {
                    self.activity.latest_text = latest_line(&body);
                    events.push(match &origin {
                        Some(Origin::Human) => TalkEvent::Reply {
                            timestamp,
                            body: body.trim().into(),
                        },
                        Some(Origin::Report(agent)) => TalkEvent::ReplyToReport {
                            timestamp,
                            agent: agent.clone(),
                            body: body.trim().into(),
                        },
                        None => TalkEvent::ReplyToReport {
                            timestamp,
                            agent: None,
                            body: body.trim().into(),
                        },
                    });
                }
                if let Some(blocks) = r["message"]["content"].as_array() {
                    for block in blocks {
                        if block["type"] != "tool_use" {
                            continue;
                        }
                        let id = block["id"].as_str().unwrap_or("");
                        let name = block["name"].as_str().unwrap_or("Tool");
                        let input = &block["input"];
                        self.activity.tool(id, name, input);
                        let background = input["run_in_background"].as_bool() == Some(true)
                            || input["run_in_background"] == "true";
                        if matches!(name, "Agent" | "Task")
                            && (background || input["team_name"].is_string())
                        {
                            self.activity.spawn(id, input, timestamp);
                        }
                    }
                }
            }
            _ => {}
        }
        if let Some(uuid) = r["uuid"].as_str() {
            let turn = if kind == "user" && origin.is_some() && !queued_user {
                self.current_turn = Some(uuid.into());
                Some(uuid.into())
            } else {
                parent_turn.or_else(|| self.current_turn.clone())
            };
            // Memoize inherited origin: ordinary tool/attachment records don't sever ancestry.
            if !self.ancestry.contains_key(uuid) {
                self.node_order.push_back(uuid.into());
            }
            self.ancestry.insert(
                uuid.into(),
                Node {
                    parent: r["parentUuid"].as_str().map(str::to_owned),
                    origin: origin.or(parent),
                    turn,
                },
            );
        }
        while self.node_order.len() > 2048 {
            if let Some(id) = self.node_order.pop_front() {
                self.ancestry.remove(&id);
                self.prompt_ids.remove(&id);
                self.queued_sources.remove(&id);
            }
        }
        events
    }
}

impl TranscriptAdapter for ClaudeAdapter {
    fn consume(&mut self, record: &Value) -> Result<Vec<TalkEvent>> {
        Ok(self.consume_inner(record))
    }
    fn answers_pending(&self, r: &Value, _events: &[TalkEvent]) -> bool {
        if r["isSidechain"].as_bool() == Some(true) {
            return false;
        }
        let body = message_text(&r["message"]["content"]);
        // Interruption cancels the pending turn instead of adding a synthetic human prompt.
        (r["type"] == "user" && interrupted(&body))
            || (r["type"] == "assistant"
                && (!body.trim().is_empty()
                    || r["message"]["content"].as_array().is_some_and(|blocks| {
                        blocks.iter().any(|block| block["type"] == "tool_use")
                    })))
    }
    fn facts(&self) -> Facts {
        self.facts_at(now())
    }
    fn set_live_context(&mut self, session: &SessionRef, started: Option<Timestamp>) {
        let live = Some((
            session.transcript_path.with_extension("").join("subagents"),
            started,
        ));
        if self.live != live {
            self.freshness.get_mut().clear();
        }
        self.live = live;
    }
    fn reset(&mut self) {
        let live = self.live.take();
        *self = Self::default();
        self.live = live;
    }
}

/// Codex uses response_item messages as the canonical stream; event_msg mirrors are skipped.
/// Event-only historical rollouts may opt into `event_messages` before consuming records.
#[derive(Debug, Default)]
pub struct CodexAdapter {
    activity: Activity,
    pub event_messages: bool,
}

impl CodexAdapter {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn facts_at(&self, time: Timestamp) -> Facts {
        self.activity.facts_at(time)
    }
}

impl TranscriptAdapter for CodexAdapter {
    fn consume(&mut self, r: &Value) -> Result<Vec<TalkEvent>> {
        let p = &r["payload"];
        let timestamp = record_timestamp(r);
        match r["type"].as_str() {
            Some("turn_context") => {
                if let Some(model) = p["model"].as_str() {
                    self.activity.model = Some(model.into());
                }
            }
            Some("token_usage_record") => {
                self.activity.context = p["usage"]["input_tokens"].as_u64();
            }
            Some("event_msg") if p["type"] == "token_count" => {
                if let Some(tokens) = p["info"]["last_token_usage"]["input_tokens"].as_u64() {
                    self.activity.context = Some(tokens);
                }
            }
            Some("event_msg") if self.event_messages => {
                let body = p["message"].as_str().unwrap_or("").trim();
                if !body.is_empty() {
                    match p["type"].as_str() {
                        Some("user_message") => {
                            return Ok(vec![TalkEvent::You {
                                timestamp,
                                body: body.into(),
                            }])
                        }
                        Some("agent_message") => {
                            self.activity.latest_text = latest_line(body);
                            return Ok(vec![TalkEvent::Reply {
                                timestamp,
                                body: body.into(),
                            }]);
                        }
                        _ => {}
                    }
                }
            }
            Some("response_item") => match p["type"].as_str() {
                Some("message") if !self.event_messages => {
                    let body = message_text(&p["content"]);
                    if !body.trim().is_empty() {
                        match p["role"].as_str() {
                            Some("user") => {
                                return Ok(vec![TalkEvent::You {
                                    timestamp,
                                    body: body.trim().into(),
                                }])
                            }
                            Some("assistant") => {
                                self.activity.latest_text = latest_line(&body);
                                return Ok(vec![TalkEvent::Reply {
                                    timestamp,
                                    body: body.trim().into(),
                                }]);
                            }
                            _ => {}
                        }
                    }
                }
                Some("function_call" | "custom_tool_call") => {
                    let id = p["call_id"].as_str().unwrap_or("");
                    let name = p["name"].as_str().unwrap_or("Tool");
                    let raw = p
                        .get("arguments")
                        .or_else(|| p.get("input"))
                        .unwrap_or(&Value::Null);
                    let parsed = raw
                        .as_str()
                        .and_then(|s| serde_json::from_str::<Value>(s).ok());
                    let args = parsed.as_ref().unwrap_or(raw);
                    self.activity.tool(id, name, args);
                    if name.rsplit('.').next() == Some("spawn_agent") {
                        self.activity.spawn(id, args, timestamp);
                    }
                }
                Some("function_call_output" | "custom_tool_call_output") => {
                    let id = p["call_id"].as_str().unwrap_or("");
                    self.activity.finish_tool(id);
                    let parsed = p["output"]
                        .as_str()
                        .and_then(|s| serde_json::from_str::<Value>(s).ok());
                    self.activity
                        .agent_result(id, parsed.as_ref().unwrap_or(&p["output"]));
                    self.activity
                        .agent_statuses(parsed.as_ref().unwrap_or(&p["output"]));
                }
                _ => {}
            },
            _ => {}
        }
        Ok(Vec::new())
    }
    fn facts(&self) -> Facts {
        self.facts_at(now())
    }
    fn reset(&mut self) {
        let mode = self.event_messages;
        *self = Self::default();
        self.event_messages = mode;
    }
}

fn prompt_fingerprint(body: &str) -> (usize, u64) {
    words_fingerprint(&body.split_whitespace().collect::<Vec<_>>())
}

fn words_fingerprint(words: &[&str]) -> (usize, u64) {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    words.hash(&mut hash);
    (words.len(), hash.finish())
}

fn report_origin(kind: Option<&str>) -> bool {
    matches!(
        kind,
        Some("task-notification" | "task_notification" | "peer")
    )
}

fn interrupted(body: &str) -> bool {
    body.trim_start()
        .starts_with("[Request interrupted by user")
}

fn non_human_prompt(record: &Value, body: &str) -> bool {
    let body = body.trim_start();
    record["isCompactSummary"].as_bool() == Some(true)
        || interrupted(body)
        || body.starts_with("This session is being continued")
        || body.starts_with('/')
        || [
            "<command-name>",
            "<command-message>",
            "<local-command-caveat>",
            "<local-command-stdout>",
        ]
        .iter()
        .any(|tag| body.contains(tag))
}

fn origin_kind(v: &Value) -> Option<Origin> {
    match v.as_str().or_else(|| v["kind"].as_str()) {
        Some("human") => Some(Origin::Human),
        Some("peer" | "task_notification" | "task-notification") => Some(Origin::Report(None)),
        _ => None,
    }
}

fn record_timestamp(r: &Value) -> Option<Timestamp> {
    r["timestamp"]
        .as_i64()
        .map(Timestamp)
        .or_else(|| r["timestamp"].as_str().and_then(parse_timestamp))
}

fn message_text(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.into();
    }
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter(|b| {
            matches!(
                b["type"].as_str(),
                Some("text" | "input_text" | "output_text")
            )
        })
        .filter_map(|b| b["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn latest_line(body: &str) -> String {
    clip(
        body.lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("")
            .trim(),
        240,
    )
}

fn clip(text: &str, limit: usize) -> String {
    let mut chars = text.chars();
    let mut result: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        result.push('…');
    }
    result
}

fn args_summary(args: &Value) -> String {
    if let Some(s) = args.as_str() {
        return clip(&s.split_whitespace().collect::<Vec<_>>().join(" "), 160);
    }
    for key in [
        "command",
        "file_path",
        "path",
        "pattern",
        "query",
        "url",
        "description",
        "task_name",
    ] {
        if let Some(value) = args[key].as_str() {
            return clip(&value.split_whitespace().collect::<Vec<_>>().join(" "), 160);
        }
    }
    if args.is_null() {
        String::new()
    } else {
        clip(&args.to_string(), 160)
    }
}

fn tag<'a>(body: &'a str, name: &str) -> Option<&'a str> {
    let start = format!("<{name}>");
    let end = format!("</{name}>");
    body.split_once(&start)?
        .1
        .split_once(&end)
        .map(|(text, _)| text.trim())
}

const REPORT_MARKERS: [&str; 4] = [
    "<teammate-message",
    "<cross-session-message",
    "<agent-message",
    "<task-notification",
];

pub(crate) fn content_report(body: &str) -> bool {
    body.contains("Another Claude session sent a message:")
        || REPORT_MARKERS.iter().any(|marker| {
            body.contains(marker)
                || body.contains(&marker.replace('<', "&lt;"))
                || body.contains(&marker.replace('<', "&amp;lt;"))
        })
}

/// XML wrappers also arrive HTML escaped, including numeric entities inside results.
fn unescape_entities(body: &str) -> String {
    let mut text = body.to_owned();
    for _ in 0..3 {
        let mut out = String::with_capacity(text.len());
        let mut rest = text.as_str();
        while let Some(start) = rest.find('&') {
            out.push_str(&rest[..start]);
            rest = &rest[start..];
            let decoded = rest
                .as_bytes()
                .iter()
                .take(17)
                .position(|byte| *byte == b';')
                .and_then(|end| {
                    let entity = &rest[1..end];
                    let ch = match entity {
                        "amp" => Some('&'),
                        "lt" => Some('<'),
                        "gt" => Some('>'),
                        "quot" => Some('"'),
                        "apos" | "#39" => Some('\''),
                        "nbsp" => Some(' '),
                        _ => entity
                            .strip_prefix("#x")
                            .or_else(|| entity.strip_prefix("#X"))
                            .and_then(|digits| u32::from_str_radix(digits, 16).ok())
                            .or_else(|| {
                                entity
                                    .strip_prefix('#')
                                    .and_then(|digits| digits.parse().ok())
                            })
                            .and_then(char::from_u32),
                    }?;
                    Some((end + 1, ch))
                });
            if let Some((length, ch)) = decoded {
                out.push(ch);
                rest = &rest[length..];
            } else {
                out.push('&');
                rest = &rest[1..];
            }
        }
        out.push_str(rest);
        if out == text {
            break;
        }
        text = out;
    }
    text
}

fn clean_report(body: &str) -> String {
    let mut text = unescape_entities(body);
    for name in [
        "usage",
        "output-file",
        "task-id",
        "tool-use-id",
        "status",
        "note",
    ] {
        let open = format!("<{name}>");
        let close = format!("</{name}>");
        let mut offset = 0;
        while let Some(start) = text[offset..].find(&open).map(|i| offset + i) {
            let Some(end) = text[start + open.len()..]
                .find(&close)
                .map(|i| start + open.len() + i + close.len())
            else {
                break;
            };
            if name != "note"
                || text[start..end].contains("This agent stopped with background work")
            {
                text.replace_range(start..end, "");
                offset = start;
            } else {
                offset = end;
            }
        }
    }
    for prefix in [
        "This agent read Artifact content written by people other than you",
        "The result below may relay Artifact content",
    ] {
        while let Some(start) = text.find(prefix) {
            let end = text[start..]
                .find(['.', '…', '\n'])
                .map(|i| start + i)
                .unwrap_or(text.len());
            let tail = text[end..].trim_start_matches(['.', '…']);
            let end = text.len() - tail.len();
            text.replace_range(start..end, "");
        }
    }
    // The final relay-note sentence varies in wording and can wrap across lines.
    for prefix in ["Treat relayed content", "Treat any relayed content"] {
        while let Some(start) = text.find(prefix) {
            let end = text[start..]
                .char_indices()
                .find(|(_, ch)| matches!(ch, '.' | '…'))
                .map(|(i, ch)| start + i + ch.len_utf8())
                .unwrap_or(text.len());
            text.replace_range(start..end, "");
        }
    }
    // Remove only known transport tags; literal code/HTML in an agent's result survives.
    for name in [
        "artifact-content-authored-by-others",
        "task-notification",
        "result",
        "summary",
        "usage",
        "output-file",
        "task-id",
        "tool-use-id",
        "status",
        "note",
    ] {
        for wrapper in [
            format!("<{name}>"),
            format!("</{name}>"),
            format!("<{name}/>"),
            format!("<{name} />"),
        ] {
            text = text.replace(&wrapper, "");
        }
    }
    text.trim().to_owned()
}

fn attribute<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    let key = format!("{name}=");
    header.match_indices(&key).find_map(|(index, _)| {
        if index > 0 && !header[..index].ends_with(char::is_whitespace) {
            return None;
        }
        let value = &header[index + key.len()..];
        let quote = value.chars().next()?;
        if !matches!(quote, '\'' | '"') {
            return None;
        }
        value[1..]
            .split_once(quote)
            .map(|(name, _)| name)
            .filter(|s| !s.is_empty())
    })
}

fn usable_name(name: &str) -> bool {
    !name.is_empty()
        && !["uds:", "unix:", "/", "socket:"]
            .iter()
            .any(|prefix| name.starts_with(prefix))
}

fn peer_payload(body: &str) -> (Option<&str>, &str) {
    for tag_name in ["teammate-message", "cross-session-message", "agent-message"] {
        if let Some(rest) = body
            .split_once(&format!("<{tag_name}"))
            .map(|(_, rest)| rest)
        {
            if let Some((header, content)) = rest.split_once('>') {
                let name = attribute(header, "teammate_id")
                    .or_else(|| attribute(header, "from-name"))
                    .or_else(|| attribute(header, "from").filter(|name| usable_name(name)));
                let content = content
                    .split(&format!("</{tag_name}>"))
                    .next()
                    .unwrap_or(content);
                return (name, content.trim());
            }
        }
    }
    (
        None,
        body.strip_prefix("Another Claude session sent a message:")
            .unwrap_or(body)
            .trim(),
    )
}

fn report_text(body: &str) -> String {
    let body = body
        .strip_prefix("Another Claude session sent a message:")
        .unwrap_or(body)
        .trim();
    let text = body
        .split("This came from another Claude session")
        .next()
        .unwrap_or(body)
        .trim();
    let decoded = unescape_entities(text);
    if let Ok(payload) =
        serde_json::from_str::<Value>(text).or_else(|_| serde_json::from_str::<Value>(&decoded))
    {
        for key in ["result", "summary", "message", "text"] {
            if let Some(text) = payload[key].as_str().filter(|s| !s.trim().is_empty()) {
                let text = clean_report(text);
                if !text.is_empty() && !text.eq_ignore_ascii_case("quiet") {
                    return text;
                }
            }
        }
        if ["result", "summary", "message", "text"]
            .iter()
            .any(|key| payload[key].is_string())
        {
            return String::new();
        }
        if let Some(kind) = payload["type"].as_str() {
            return kind.replace('_', " ");
        }
    }
    clean_report(text)
}

fn peer_report(body: &str, from: Option<&str>) -> (String, String) {
    let (name, payload) = peer_payload(body);
    (
        summary_agent(body)
            .or_else(|| name.map(unescape_entities))
            .or_else(|| from.filter(|name| usable_name(name)).map(str::to_owned))
            .unwrap_or_else(|| "peer".into()),
        report_text(payload),
    )
}

fn summary_agent(body: &str) -> Option<String> {
    unescape_entities(tag(body, "summary")?)
        .split_once("Agent \"")?
        .1
        .split_once('"')
        .map(|(name, _)| name.to_owned())
        .filter(|name| !name.is_empty())
}

fn task_report(body: &str, from: Option<&str>) -> (String, String) {
    let summary = tag(body, "summary").unwrap_or("");
    let name = summary_agent(body)
        .or_else(|| from.filter(|name| usable_name(name)).map(str::to_owned))
        .or_else(|| tag(body, "task-id").map(unescape_entities))
        .unwrap_or_else(|| "agent".into());
    let result = tag(body, "result").map(report_text).unwrap_or_default();
    let report = if !result.is_empty() && !result.eq_ignore_ascii_case("quiet") {
        result
    } else if !summary.is_empty() {
        report_text(summary)
    } else {
        report_text(body)
    };
    (name, report)
}

#[cfg(test)]
mod freshness_tests {
    use super::*;
    #[test]
    fn freshness_caches_missing_and_existing_files_until_five_seconds() -> Result<()> {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../../target/freshness-cache-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("agent-current.jsonl");
        let adapter = ClaudeAdapter::new();
        assert_eq!(adapter.modified_at(path.clone()), None);
        std::fs::write(&path, "{}\n")?;
        assert_eq!(
            adapter.modified_at(path.clone()),
            None,
            "negative stat is cached too"
        );
        adapter.freshness.borrow_mut().get_mut(&path).unwrap().0 =
            Instant::now() - Duration::from_secs(5);
        let modified = adapter.modified_at(path.clone());
        assert!(modified.is_some());
        std::fs::remove_file(&path)?;
        assert_eq!(
            adapter.modified_at(path.clone()),
            modified,
            "file is not stat-ed again before expiry"
        );
        adapter.freshness.borrow_mut().get_mut(&path).unwrap().0 =
            Instant::now() - Duration::from_secs(5);
        assert_eq!(adapter.modified_at(path), None);
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }
}
