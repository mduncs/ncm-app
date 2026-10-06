use anyhow::Result;
use ncm_model::{Facts, NowKind, SessionRef, SubagentState, TalkEvent, Timestamp};
use ncm_talk::{
    harness_for_command, merge_facts, parse_timestamp, screen_facts, ClaudeAdapter, CodexAdapter,
    ProcessTree, SessionDiscovery, TalkBlock, TalkView, TranscriptAdapter, TranscriptFollower,
};
use serde_json::{json, Value};
use std::{
    fs,
    io::Write,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/talk-fixtures")
            .join(format!(
                "integration-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn session(&self) -> SessionRef {
        SessionRef {
            harness: harness_for_command("claude").unwrap(),
            session_id: "fixture".into(),
            transcript_path: self.0.join("fixture.jsonl"),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn consume(adapter: &mut dyn TranscriptAdapter, records: &[Value]) -> Vec<TalkEvent> {
    records
        .iter()
        .flat_map(|r| adapter.consume(r).unwrap())
        .collect()
}

fn jsonl_fixture(text: &str) -> Vec<Value> {
    text.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn queued_report_fixtures_preserve_attribution_and_replies_clear_attention() -> Result<()> {
    let fixture = Fixture::new();
    let session = fixture.session();
    let shapes = jsonl_fixture(include_str!("fixtures/queued-report.jsonl"));
    for (body, name, expected) in [
        (shapes[2]["attachment"]["prompt"].as_str().unwrap(), "Control orchestrator (relaunch 21:17)", "Checked one file."),
        ("<teammate-message teammate_id=\"peer\">Checked.</teammate-message>", "peer", "Checked."),
        ("<cross-session-message from=\"uds:/tmp/invented.sock\" from-name=\"peer\">Checked.</cross-session-message>", "peer", "Checked."),
        ("<agent-message from=\"peer\">Checked.</agent-message>", "peer", "Checked."),
        ("Another Claude session sent a message:\nChecked.\nThis came from another Claude session…", "peer", "Checked."),
        ("&lt;task-notification&gt;&lt;summary&gt;Agent &quot;peer&quot; finished&lt;/summary&gt;&lt;result&gt;Checked.&lt;/result&gt;&lt;/task-notification&gt;", "peer", "Checked."),
    ] {
        let mut records = shapes.clone();
        records[2]["attachment"]["prompt"] = json!(body);
        records[3]["message"]["content"][0]["text"] = json!(body);
        // An attachment may repeat before the corresponding user record is saved.
        records.insert(3, records[2].clone());
        for reverse_delivery in [false, true] {
            let mut records = records.clone();
            if reverse_delivery {
                // Some harness versions persist the resulting user prompt first.
                records.swap(2, 4);
            }
            let mut file = fs::File::create(&session.transcript_path)?;
            for record in &records { writeln!(file, "{record}")?; }
            drop(file);
            let mut raw = ClaudeAdapter::new();
            let expected_events = consume(&mut raw, &records);
            let mut projected = ClaudeAdapter::new();
            let mut follower = TranscriptFollower::new(&session);
            let events = follower.poll_adapter(&mut projected)?.events;
            assert_eq!(events, expected_events);
            assert_eq!(events.len(), 3, "{events:?}");
            assert!(matches!(&events[0], TalkEvent::You { .. }));
            assert!(matches!(&events[1], TalkEvent::Report { agent, body, .. } if agent == name && body == expected));
            assert!(matches!(&events[2], TalkEvent::ReplyToReport { agent: Some(agent), .. } if agent == name));
            assert!(TalkView::from_events(&events, Timestamp(1000)).unanswered.is_none());
            assert_eq!(follower.malformed_lines, 0);
        }
    }
    // A queued prompt can be persisted only as a user record.
    let mut records = shapes.clone();
    records.remove(2);
    let events = consume(&mut ClaudeAdapter::new(), &records);
    assert!(matches!(&events[1], TalkEvent::Report { .. }));
    assert!(matches!(&events[2], TalkEvent::ReplyToReport { .. }));
    Ok(())
}

#[test]
fn report_preview_fixtures_strip_wrappers_and_fall_back_to_summary() -> Result<()> {
    let fixture = Fixture::new();
    let session = fixture.session();
    let text = include_str!("fixtures/report-previews.jsonl");
    fs::write(&session.transcript_path, text)?;
    let records = jsonl_fixture(text);
    let raw = consume(&mut ClaudeAdapter::new(), &records);
    let events = TranscriptFollower::new(&session)
        .poll_adapter(&mut ClaudeAdapter::new())?
        .events;
    assert_eq!(events, raw);
    assert_eq!(events.len(), 11);
    for (event, expected) in events.iter().zip([
        "Checked A & B; 界 'ok'.",
        "Agent \"worker\" failed",
        "Agent \"worker\" finished",
        "Agent \"worker\" finished",
        "Checked & saved.",
        "Checked \"A\". <b>Kept</b> &unknown;",
        "Checked control state.",
        "Control orchestrator (worker) finished",
        "Saved summary.",
        "Updated checks.",
        "Complete.",
    ]) {
        assert!(matches!(event, TalkEvent::Report { .. }));
        assert_eq!(event.body(), expected);
    }
    assert!(matches!(&events[5], TalkEvent::Report { agent, .. } if agent == "worker"));
    Ok(())
}

#[test]
fn claude_origins_parent_walk_and_explicit_turn_origin() {
    let mut adapter = ClaudeAdapter::new();
    let events = consume(
        &mut adapter,
        &[
            json!({"type":"user","uuid":"h","origin":{"kind":"human"},"promptSource":"suggestion_accepted","message":{"content":[{"type":"text","text":"human"}]}}),
            json!({"type":"assistant","uuid":"tool","parentUuid":"h","message":{"content":[{"type":"tool_use","id":"read","name":"Read","input":{"file_path":"a.rs"}}]}}),
            json!({"type":"user","uuid":"result","parentUuid":"tool","message":{"content":[{"type":"tool_result","tool_use_id":"read","content":"uninteresting"}]}}),
            json!({"type":"assistant","uuid":"a","parentUuid":"result","message":{"content":[{"type":"text","text":"reply"}]}}),
            json!({"type":"user","uuid":"p","origin":{"kind":"peer"},"message":{"content":"<agent-message from=\"worker\">report body</agent-message>"}}),
            json!({"type":"attachment","uuid":"env","parentUuid":"p","attachment":{"type":"environment","snapshot":{}}}),
            json!({"type":"assistant","uuid":"r","parentUuid":"env","message":{"content":[{"type":"text","text":"report reply"}]}}),
            json!({"type":"assistant","parentUuid":"r","turnOrigin":"human","message":{"content":[{"type":"text","text":"content report wins"}]}}),
            json!({"type":"assistant","parentUuid":"h","turnOrigin":"task_notification","message":{"content":[{"type":"text","text":"explicit report wins"}]}}),
            json!({"type":"assistant","isSidechain":true,"turnOrigin":"human","message":{"content":[{"type":"text","text":"skip"}]}}),
        ],
    );
    assert_eq!(events.len(), 6);
    assert!(matches!(&events[0], TalkEvent::You { body, .. } if body == "human"));
    assert!(matches!(&events[1], TalkEvent::Reply { body, .. } if body == "reply"));
    assert!(
        matches!(&events[2], TalkEvent::Report { agent, body, .. } if agent == "worker" && body == "report body")
    );
    assert!(
        matches!(&events[3], TalkEvent::ReplyToReport { agent: Some(agent), .. } if agent == "worker")
    );
    assert!(matches!(&events[4], TalkEvent::ReplyToReport { .. }));
    assert!(matches!(&events[5], TalkEvent::ReplyToReport { .. }));
}

#[test]
fn queued_prompts_redirect_mid_turn_and_legacy_meta_is_skipped() {
    let mut adapter = ClaudeAdapter::new();
    let events = consume(
        &mut adapter,
        &[
            json!({"type":"user","uuid":"legacy","message":{"content":"old human"}}),
            json!({"type":"user","isMeta":true,"message":{"content":"meta"}}),
            json!({"type":"user","message":{"content":[{"type":"tool_result","content":"never human"}]}}),
            json!({"type":"user","uuid":"report","origin":{"kind":"task-notification"},"message":{"content":"<summary>Agent \"worker\" finished</summary><status>completed</status>"}}),
            json!({"type":"attachment","uuid":"q","parentUuid":"report","timestamp":"2026-10-03T00:00:00Z","attachment":{"type":"queued_command","source_uuid":"queued-id","prompt":"queued"}}),
            json!({"type":"attachment","attachment":{"type":"queued_command","source_uuid":"queued-id","prompt":"queued"}}),
            json!({"type":"assistant","uuid":"a","parentUuid":"report","turnOrigin":"task_notification","message":{"content":[{"type":"text","text":"answer queued"}]}}),
            json!({"type":"user","uuid":"report2","origin":{"kind":"peer"},"message":{"content":"<agent-message from=\"peer\">another</agent-message>"}}),
            json!({"type":"assistant","parentUuid":"report2","turnOrigin":"peer","message":{"content":[{"type":"text","text":"ack"}]}}),
        ],
    );
    assert_eq!(events.len(), 6);
    assert!(matches!(&events[2], TalkEvent::You { body, .. } if body == "queued"));
    assert!(matches!(&events[3], TalkEvent::Reply { .. }));
    assert!(matches!(&events[5], TalkEvent::ReplyToReport { .. }));
    assert!(TalkView::from_events(&events, Timestamp(10))
        .unanswered
        .is_none());
    let events = adapter.consume(&json!({"type":"attachment","attachment":{"type":"queued_command","prompt":"new question"}})).unwrap();
    assert!(TalkView::from_events(&events, Timestamp(10))
        .unanswered
        .is_some());
}

#[test]
fn pending_tools_usage_and_background_agents_are_independent() {
    let mut adapter = ClaudeAdapter::new();
    consume(
        &mut adapter,
        &[
            json!({"type":"assistant","timestamp":"1970-01-01T00:00:01Z","message":{"model":"test-model","usage":{"input_tokens":10,"cache_read_input_tokens":20,"cache_creation_input_tokens":30,"output_tokens":999},"content":[{"type":"text","text":"starting\nlatest text"},{"type":"tool_use","id":"spawn","name":"Agent","input":{"name":"worker","description":"inspect","run_in_background":"true"}}]}}),
        ],
    );
    let facts = adapter.facts_at(Timestamp(6000));
    assert_eq!(facts.context_tokens, Some(60));
    assert_eq!(facts.model.as_deref(), Some("test-model"));
    assert_eq!(facts.now.kind, NowKind::Tool);
    assert_eq!(facts.now.text, "Agent(inspect)");
    assert_eq!(
        facts.subagents[0].running_time,
        Some(Duration::from_secs(5))
    );
    consume(
        &mut adapter,
        &[
            json!({"type":"user","toolUseResult":{"agentId":"agent-1"},"message":{"content":[{"type":"tool_result","tool_use_id":"spawn","content":"launched"}]}}),
        ],
    );
    assert_eq!(adapter.facts().now.text, "latest text");
    assert_eq!(adapter.facts().busy_count(), 1);
    let events = consume(
        &mut adapter,
        &[
            json!({"type":"user","origin":{"kind":"task-notification"},"message":{"content":"<task-id>agent-1</task-id><tool-use-id>spawn</tool-use-id><status>completed</status><summary>Agent \"worker\" finished</summary><result>done work</result>"}}),
        ],
    );
    assert_eq!(adapter.facts().subagents.len(), 1);
    assert_eq!(adapter.facts().subagents[0].state, SubagentState::Idle);
    assert!(events[0].body().contains("done work"));
    adapter.reset();
    assert_eq!(adapter.facts(), Facts::default());
}

#[test]
fn codex_canonical_messages_ignore_event_mirrors_and_reports() {
    let mut adapter = CodexAdapter::new();
    let events = consume(
        &mut adapter,
        &[
            json!({"type":"turn_context","payload":{"model":"codex-test"}}),
            json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"ask"}]}}),
            json!({"type":"event_msg","payload":{"type":"user_message","message":"ask"}}),
            json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"answer"}]}}),
            json!({"type":"event_msg","payload":{"type":"agent_message","message":"answer"}}),
            json!({"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"hidden"}]}}),
            json!({"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":123,"cached_input_tokens":100,"output_tokens":999}}}}),
            json!({"type":"response_item","payload":{"type":"function_call","call_id":"c","name":"exec_command","arguments":"{\"command\":\"ls\"}"}}),
        ],
    );
    assert_eq!(events.len(), 2);
    assert!(matches!(&events[0], TalkEvent::You { .. }));
    assert!(matches!(&events[1], TalkEvent::Reply { .. }));
    assert_eq!(adapter.facts().context_tokens, Some(123));
    assert_eq!(adapter.facts().now.text, "exec_command(ls)");
    adapter.consume(&json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"c","output":"done"}})).unwrap();
    assert_eq!(adapter.facts().now.text, "answer");
    adapter.reset();
    assert_eq!(adapter.facts(), Facts::default());
}

#[test]
fn screen_fallback_matches_prototype_precedence_and_agent_rows() {
    let lines = [
        "\x1b[31mModel: Opus 4.6\x1b[0m",
        "project | /tmp Ctx: 12.5k",
        "answer text",
        "Agents",
        "● worker  2m 3s",
        "○ other  idle",
        "3 idle agents",
        "↓ 2 more",
        "> prompt",
    ];
    let facts = screen_facts(&lines);
    assert_eq!(facts.now.text, "answer text");
    assert_eq!(facts.model.as_deref(), Some("Opus 4.6"));
    assert_eq!(facts.context_tokens, Some(12500));
    assert_eq!(facts.subagents.len(), 2);
    assert_eq!(facts.hidden_count, 5);
    assert_eq!(facts.busy_count(), 1);
    assert_eq!(
        facts.subagents[0].running_time,
        Some(Duration::from_secs(123))
    );
    assert_eq!(
        screen_facts(&["⏺ Bash(ls)", "* Thinking… (1s)"]).now.kind,
        NowKind::Spinner
    );
    assert_eq!(
        screen_facts(&["⏺ Bash(ls)", "ordinary"]).now.kind,
        NowKind::Tool
    );
    let merged = merge_facts(
        Facts {
            model: Some("transcript".into()),
            context_tokens: Some(0),
            ..Facts::default()
        },
        facts,
    );
    assert_eq!(merged.model.as_deref(), Some("transcript"));
    assert_eq!(merged.context_tokens, Some(0));
    assert_eq!(merged.now.text, "answer text");
}

#[test]
fn process_tree_names_spaces_and_nested_harnesses() {
    let tree = ProcessTree::parse("1 0 /bin/zsh\n2 1 /Applications/A Folder/codex\n3 2 /bin/sh\n4 3 claude\n5 0 codex-code-mode-host\n6 0 aider\n");
    assert_eq!(
        tree.descendants(1)
            .iter()
            .map(|p| p.pid)
            .collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
    assert_eq!(tree.harness(1).unwrap().name, "codex");
    assert!(tree.harness(5).is_none());
    assert_eq!(tree.harness(6).unwrap().name, "aider");
    let ambiguous = ProcessTree::parse("1 0 sh\n2 1 claude\n3 1 codex\n");
    assert!(ambiguous.harness(1).is_none());
    let cyclic = ProcessTree::parse("1 3 zsh\n2 1 claude\n3 2 sh\n");
    assert_eq!(
        cyclic
            .descendants(1)
            .iter()
            .map(|p| p.pid)
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
}

#[test]
fn discovery_obeys_exact_session_id_not_newest_project_file() -> Result<()> {
    let fixture = Fixture::new();
    let claude = fixture.0.join("claude");
    fs::create_dir_all(claude.join("sessions"))?;
    fs::create_dir_all(claude.join("projects/project"))?;
    fs::write(
        claude.join(format!("sessions/{}.json", std::process::id())),
        json!({"pid": std::process::id(), "sessionId": "owned"}).to_string(),
    )?;
    fs::write(claude.join("projects/project/owned.jsonl"), "{}\n")?;
    fs::write(
        claude.join("projects/project/another-process.jsonl"),
        "{}\n",
    )?;
    let discovery = SessionDiscovery {
        claude_root: claude.clone(),
        codex_root: fixture.0.join("codex"),
    };
    let tree = ProcessTree::parse(&format!("1 0 zsh\n{} 1 claude\n", std::process::id()));
    let session = discovery.discover_with_tree(1, &tree)?.unwrap();
    assert_eq!(session.session_id, "owned");
    fs::write(
        claude.join(format!("sessions/{}.json", std::process::id())),
        json!({"pid": std::process::id(), "sessionId": "missing"}).to_string(),
    )?;
    assert!(discovery.discover_with_tree(1, &tree)?.is_none());
    assert!(discovery.session("../bad").is_err());
    Ok(())
}

#[test]
fn follower_skips_malformed_and_oversize_but_waits_on_partial_then_resets() -> Result<()> {
    let fixture = Fixture::new();
    let session = fixture.session();
    fs::write(&session.transcript_path, b"bad\n{}\n{\"a\":1}\n{\"a\":")?;
    let mut follower = TranscriptFollower::new(&session);
    follower.max_line_bytes = 7;
    assert_eq!(follower.poll()?.records, vec![json!({})]);
    assert_eq!(follower.malformed_lines, 2);
    let offset = follower.offset();
    assert!(follower.poll()?.records.is_empty());
    assert_eq!(offset, follower.offset());
    fs::OpenOptions::new()
        .append(true)
        .open(&session.transcript_path)?
        .write_all(b"2}\n")?;
    assert!(follower.poll()?.records.is_empty());
    assert_eq!(follower.malformed_lines, 3);
    let replacement = fixture.0.join("new.jsonl");
    fs::write(&replacement, b"{\"b\":0}\n")?;
    fs::rename(replacement, &session.transcript_path)?;
    let batch = follower.poll()?;
    assert!(batch.reset);
    assert_eq!(follower.malformed_lines, 4);
    follower.max_line_bytes = 1024;
    fs::write(&session.transcript_path, b"{}\n")?;
    let batch = follower.poll()?;
    assert!(batch.reset);
    assert_eq!(batch.records, vec![json!({})]);
    Ok(())
}

#[test]
fn streaming_follower_resets_adapter_before_replacement() -> Result<()> {
    let fixture = Fixture::new();
    let session = fixture.session();
    fs::write(
        &session.transcript_path,
        "{\"type\":\"user\",\"uuid\":\"human\",\"message\":{\"content\":\"hello\"}}\n",
    )?;
    let mut follower = TranscriptFollower::new(&session);
    let mut adapter = ClaudeAdapter::new();
    assert!(matches!(
        &follower.poll_adapter(&mut adapter)?.events[0],
        TalkEvent::You { .. }
    ));
    let replacement = fixture.0.join("next.jsonl");
    fs::write(&replacement, "{\"type\":\"assistant\",\"parentUuid\":\"human\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"new\"}]}}\n")?;
    fs::rename(replacement, &session.transcript_path)?;
    let batch = follower.poll_adapter(&mut adapter)?;
    assert!(batch.reset);
    assert!(matches!(&batch.events[0], TalkEvent::ReplyToReport { .. }));
    Ok(())
}

#[test]
fn report_runs_and_summaries_preserve_transcript_order() {
    let events = vec![
        TalkEvent::Report {
            timestamp: Some(Timestamp(10)),
            agent: "a".into(),
            body: "old".into(),
        },
        TalkEvent::ReplyToReport {
            timestamp: None,
            agent: Some("a".into()),
            body: "ack".into(),
        },
        TalkEvent::You {
            timestamp: Some(Timestamp(100)),
            body: "question".into(),
        },
        TalkEvent::Report {
            timestamp: Some(Timestamp(20)),
            agent: "a".into(),
            body: "latest".into(),
        },
        TalkEvent::Report {
            timestamp: Some(Timestamp(30)),
            agent: "b".into(),
            body: "other".into(),
        },
    ];
    let view = TalkView::from_events(&events, Timestamp(30_000));
    assert_eq!(
        view.blocks[0],
        TalkBlock::Reports {
            reports: 1,
            replies: 1
        }
    );
    assert_eq!(
        view.blocks[2],
        TalkBlock::Reports {
            reports: 2,
            replies: 0
        }
    );
    assert_eq!(view.by_agent[0].agent, "b");
    assert_eq!(view.by_agent[1].count, 2);
    assert_eq!(view.by_agent[1].latest_body, "latest");
    assert!(view.unanswered.unwrap().show_card_badge());
}

#[test]
fn timestamps_handle_offsets_fractions_and_invalid_dates() {
    assert_eq!(parse_timestamp("1970-01-01T00:00:00Z"), Some(Timestamp(0)));
    assert_eq!(
        parse_timestamp("1970-01-01T01:00:01.123456+01:00"),
        Some(Timestamp(1123))
    );
    assert_eq!(
        parse_timestamp("1969-12-31T18:00:00-06:00"),
        Some(Timestamp(0))
    );
    assert!(parse_timestamp("2026-02-29T00:00:00Z").is_none());
    assert!(parse_timestamp("2026-01-01T25:00:00Z").is_none());
}

#[test]
fn streaming_projection_matches_raw_adapter_and_drops_unused_fields() -> Result<()> {
    let fixture = Fixture::new();
    let session = fixture.session();
    let records = [
        json!({"type":"attachment","attachment":{"type":"environment","snapshot":{"unused": "x".repeat(1000)}}}),
        json!({"type":"user","uuid":"h","origin":{"kind":"human"},"message":{"content":"hello"}}),
        json!({"type":"assistant","uuid":"s","parentUuid":"h","timestamp":1000,"message":{"model":"model","usage":{"input_tokens":1},"content":[{"type":"thinking","thinking":"skip"},{"type":"text","text":"reply"},{"type":"tool_use","id":"agent","name":"Agent","input":{"name":"a","run_in_background":true}}]}}),
        json!({"type":"user","uuid":"t","parentUuid":"s","toolUseResult":{"agentId":"id-a","stdout":"unused"},"message":{"content":[{"type":"tool_result","tool_use_id":"agent","content":"unused output"}]}}),
        json!({"type":"user","origin":{"kind":"task-notification"},"message":{"content":"<task-id>id-a</task-id><summary>Agent \"a\" finished</summary><status>completed</status>"}}),
    ];
    let mut file = fs::File::create(&session.transcript_path)?;
    for record in &records {
        writeln!(file, "{record}")?;
    }
    drop(file);
    let mut raw = ClaudeAdapter::new();
    let expected = consume(&mut raw, &records);
    let mut projected = ClaudeAdapter::new();
    let mut follower = TranscriptFollower::new(&session);
    assert_eq!(follower.poll_adapter(&mut projected)?.events, expected);
    assert_eq!(
        projected.facts_at(Timestamp(2000)),
        raw.facts_at(Timestamp(2000))
    );
    assert_eq!(follower.malformed_lines, 0);
    Ok(())
}

#[test]
#[cfg(target_os = "macos")]
fn codex_discovery_uses_native_fd_paths_and_rejects_multiple_rollouts() -> Result<()> {
    let fixture = Fixture::new();
    let sessions = fixture.0.join("codex/sessions/2026/10/03");
    fs::create_dir_all(&sessions)?;
    let path = sessions.join("rollout-test-owned.jsonl");
    fs::write(
        &path,
        "{\"type\":\"session_meta\",\"payload\":{\"id\":\"owned\"}}\n",
    )?;
    let _open = fs::File::open(&path)?;
    let pid = std::process::id();
    let tree = ProcessTree::parse(&format!("{pid} 0 codex\n"));
    let discovery = SessionDiscovery {
        claude_root: fixture.0.join("claude"),
        codex_root: fixture.0.join("codex"),
    };
    let session = discovery.discover_with_tree(pid, &tree)?.unwrap();
    assert_eq!(session.session_id, "owned");
    assert_eq!(session.transcript_path, fs::canonicalize(&path)?);
    // A distinct root avoids the two-second cache without sleeping or touching a real process.
    let second_root = fixture.0.join("another/sessions");
    fs::create_dir_all(&second_root)?;
    let path2 = second_root.join("rollout-second.jsonl");
    fs::write(&path2, "{}\n")?;
    let _open2 = fs::File::open(path2)?;
    let discovery = SessionDiscovery {
        claude_root: fixture.0.join("claude"),
        codex_root: fixture.0.join("another"),
    };
    assert!(discovery.discover_with_tree(pid, &tree)?.is_none());
    Ok(())
}

#[test]
fn peer_idle_notifications_and_shell_tasks_do_not_invent_busy_subagents() {
    let mut adapter = ClaudeAdapter::new();
    consume(
        &mut adapter,
        &[
            json!({"type":"user","origin":{"kind":"task-notification"},"message":{"content":"<task-id>bash-task</task-id><status>completed</status><summary>Background command completed</summary>"}}),
            json!({"type":"user","timestamp":1000,"origin":{"kind":"peer"},"message":{"content":"<agent-message from=\"teammate\">working</agent-message>"}}),
        ],
    );
    assert_eq!(adapter.facts().subagents.len(), 1);
    assert_eq!(adapter.facts().busy_count(), 1);
    consume(
        &mut adapter,
        &[
            json!({"type":"user","origin":{"kind":"peer"},"message":{"content":"<agent-message from=\"teammate\">{\"type\": \"idle_notification\"}</agent-message>"}}),
        ],
    );
    assert_eq!(adapter.facts().subagents.len(), 1);
    assert_eq!(adapter.facts().busy_count(), 0);
}

#[test]
fn queued_redirect_is_scoped_to_turn_and_survives_duplicate_delivery() {
    let mut adapter = ClaudeAdapter::new();
    consume(
        &mut adapter,
        &[
            json!({"type":"user","uuid":"old","origin":{"kind":"peer"},"message":{"content":"<agent-message from=\"a\">old</agent-message>"}}),
            json!({"type":"user","uuid":"current","origin":{"kind":"peer"},"message":{"content":"<agent-message from=\"b\">current</agent-message>"}}),
            json!({"type":"attachment","parentUuid":"current","attachment":{"type":"queued_command","source_uuid":"q","prompt":"queue"}}),
            json!({"type":"user","uuid":"q","parentUuid":"current","origin":{"kind":"human"},"promptSource":"queued","message":{"content":"queue"}}),
        ],
    );
    let events = consume(
        &mut adapter,
        &[
            json!({"type":"assistant","parentUuid":"old","turnOrigin":"peer","message":{"content":[{"type":"text","text":"old reply"}]}}),
            json!({"type":"assistant","parentUuid":"q","turnOrigin":"peer","message":{"content":[{"type":"text","text":"queued reply"}]}}),
        ],
    );
    assert!(matches!(&events[0], TalkEvent::ReplyToReport { .. }));
    assert!(matches!(&events[1], TalkEvent::Reply { .. }));
}

#[test]
fn bounded_polls_drain_unchanged_file_and_same_length_rewrites_reset() -> Result<()> {
    let fixture = Fixture::new();
    let session = fixture.session();
    fs::write(
        &session.transcript_path,
        b"{\"a\":1}\n{\"a\":2}\n{\"a\":3}\n",
    )?;
    let mut follower = TranscriptFollower::new(&session);
    follower.max_records = 1;
    for n in 1..=3 {
        assert_eq!(follower.poll()?.records, vec![json!({"a":n})]);
    }
    assert!(follower.poll()?.records.is_empty());
    fs::write(
        &session.transcript_path,
        b"{\"b\":1}\n{\"b\":2}\n{\"b\":3}\n",
    )?;
    let batch = follower.poll()?;
    assert!(batch.reset);
    assert_eq!(batch.records, vec![json!({"b":1})]);
    Ok(())
}

#[test]
fn codex_spawn_and_finished_wait_output_survive_streaming_projection() -> Result<()> {
    let fixture = Fixture::new();
    let session = fixture.session();
    let records = [
        json!({"type":"response_item","timestamp":1000,"payload":{"type":"function_call","call_id":"spawn","name":"collaboration.spawn_agent","arguments":"{\"task_name\":\"worker\"}"}}),
        json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"spawn","output":"{\"agent_id\":\"a\",\"nickname\":\"worker\"}"}}),
        json!({"type":"response_item","payload":{"type":"function_call","call_id":"wait","name":"collaboration.wait_agent","arguments":"{}"}}),
        json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"wait","output":"{\"status\":{\"a\":{\"completed\":\"done\"}}}"}}),
    ];
    let mut file = fs::File::create(&session.transcript_path)?;
    for record in &records {
        writeln!(file, "{record}")?;
    }
    drop(file);
    let mut raw = CodexAdapter::new();
    consume(&mut raw, &records);
    let mut projected = CodexAdapter::new();
    let mut follower = TranscriptFollower::new(&session);
    follower.poll_adapter(&mut projected)?;
    assert_eq!(
        projected.facts_at(Timestamp(2000)),
        raw.facts_at(Timestamp(2000))
    );
    assert_eq!(projected.facts().subagents[0].name, "worker");
    assert_eq!(projected.facts().busy_count(), 0);
    assert_eq!(follower.malformed_lines, 0);
    Ok(())
}

#[test]
fn metadata_free_and_human_origin_envelopes_start_report_turns() {
    for origin in [
        json!(null),
        json!({"kind":"human"}),
        json!({"kind":"peer"}),
        json!({"kind":"task-notification"}),
    ] {
        for (envelope, name, expected) in [
            ("<teammate-message teammate_id=\"look-objects\" color=\"purple\">ready</teammate-message>", "look-objects", "ready"),
            ("<cross-session-message from=\"uds:/tmp/cc-socks/7.sock\" from-name=\"named worker\">{\"type\":\"idle_notification\",\"result\":\"done\"}</cross-session-message>", "named worker", "done"),
            ("<agent-message from=\"helper\">{\"summary\":\"checked\"}</agent-message>", "helper", "checked"),
            ("<task-notification><summary>Agent \"builder\" finished</summary><result>{\"result\":\"built\"}</result></task-notification>", "builder", "built"),
        ] {
            let mut adapter = ClaudeAdapter::new();
            let events = consume(&mut adapter, &[
                json!({"type":"user","uuid":"h","message":{"content":"typed"}}),
                json!({"type":"user","uuid":"r","origin":origin,"message":{"content":format!("Another Claude session sent a message:\n{envelope}\n\nThis came from another Claude session — not typed by your user.")}}),
                json!({"type":"assistant","uuid":"a","parentUuid":"r","turnOrigin":"human","message":{"content":[{"type":"text","text":"ack"}]}}),
            ]);
            assert!(matches!(&events[1], TalkEvent::Report { agent, body, .. } if agent == name && body == expected), "{:?}", events[1]);
            assert!(matches!(&events[2], TalkEvent::ReplyToReport { agent: Some(agent), .. } if agent == name));
            assert!(TalkView::from_events(&events, Timestamp(1000)).unanswered.is_none());
            if name != "builder" { assert_eq!(adapter.facts().subagents[0].name, name); }
        }
    }
    let mut adapter = ClaudeAdapter::new();
    let events = consume(
        &mut adapter,
        &[
            json!({"type":"user","origin":{"kind":"human","from":"sender"},"message":{"content":"Another Claude session sent a message:\nhello\n\nThis came from another Claude session…"}}),
            json!({"type":"user","origin":{"kind":"peer","from":"uds:/tmp/socket"},"message":{"content":"raw"}}),
            json!({"type":"user","message":{"content":"embedded <agent-message from='embedded'>body</agent-message>"}}),
        ],
    );
    assert!(
        matches!(&events[0], TalkEvent::Report {agent, body,..} if agent == "sender" && body == "hello")
    );
    assert!(matches!(&events[1], TalkEvent::Report {agent,..} if agent == "peer"));
    assert!(
        matches!(&events[2], TalkEvent::Report {agent, body,..} if agent == "embedded" && body == "body")
    );
}

#[test]
fn live_agents_require_current_launch_recent_own_transcript_and_no_finish() -> Result<()> {
    let fixture = Fixture::new();
    let session = fixture.session();
    let directory = session.transcript_path.with_extension("").join("subagents");
    fs::create_dir_all(&directory)?;
    let path = directory.join("agent-id-fresh.jsonl");
    fs::write(&path, "{}\n")?;
    let time = ncm_talk::now();
    let start = Timestamp(time.0 - 10000);
    let mut adapter = ClaudeAdapter::new();
    adapter.set_live_context(&session, Some(start));
    for (id, name, timestamp) in [
        ("old", "historical", start.0 - 1),
        ("fresh", "current", start.0 + 1),
        ("missing", "no-file", start.0 + 1),
    ] {
        consume(
            &mut adapter,
            &[
                json!({"type":"assistant","timestamp":timestamp,"message":{"content":[{"type":"tool_use","id":id,"name":"Agent","input":{"name":name,"run_in_background":true}}]}}),
                json!({"type":"user","toolUseResult":{"agentId":format!("id-{id}")},"message":{"content":[{"type":"tool_result","tool_use_id":id,"content":"launched"}]}}),
            ],
        );
    }
    consume(
        &mut adapter,
        &[
            json!({"type":"user","timestamp":time.0,"message":{"content":"<task-notification><task-id>id-old</task-id><summary>Agent \"historical\" finished</summary><status>completed</status></task-notification>"}}),
        ],
    );
    let facts = adapter.facts_at(time);
    assert_eq!(facts.subagents.len(), 2);
    assert_eq!(facts.busy_count(), 1);
    assert_eq!(adapter.facts_at(Timestamp(time.0 + 181000)).busy_count(), 0);
    consume(
        &mut adapter,
        &[
            json!({"type":"user","timestamp":time.0,"message":{"content":"<teammate-message teammate_id=\"current\">{\"type\":\"idle_notification\",\"result\":\"done\"}</teammate-message>"}}),
            json!({"type":"user","timestamp":time.0,"message":{"content":"<teammate-message teammate_id=\"current\">another report</teammate-message>"}}),
        ],
    );
    assert_eq!(adapter.facts_at(time).busy_count(), 0);
    let screen = screen_facts(&[
        "○ current  testing idle task…  29m 20s · ↓ 239.9k tokens",
        "○ no-file  task description  idle · ↓ 10k tokens",
    ]);
    let merged = merge_facts(adapter.facts_at(time), screen);
    assert_eq!(merged.busy_count(), 1);
    assert_eq!(
        merged
            .subagents
            .iter()
            .find(|a| a.name == "current")
            .unwrap()
            .running_time,
        Some(Duration::from_secs(1760))
    );
    // Reset preserves the live context, so resumed history cannot reintroduce stale busy agents.
    adapter.reset();
    consume(
        &mut adapter,
        &[
            json!({"type":"assistant","timestamp":start.0-1,"message":{"content":[{"type":"tool_use","id":"old","name":"Agent","input":{"name":"old","run_in_background":true}}]}}),
        ],
    );
    assert!(adapter.facts_at(time).subagents.is_empty());
    adapter.set_live_context(&session, None);
    consume(
        &mut adapter,
        &[
            json!({"type":"assistant","timestamp":time.0,"message":{"content":[{"type":"tool_use","id":"fresh","name":"Agent","input":{"name":"current","run_in_background":true}}]}}),
        ],
    );
    assert_eq!(adapter.facts_at(time).busy_count(), 0);
    Ok(())
}

#[test]
fn streaming_report_projection_and_bounded_catchup_keep_newest_text() -> Result<()> {
    let fixture = Fixture::new();
    let session = fixture.session();
    let mut file = fs::File::create(&session.transcript_path)?;
    writeln!(
        file,
        "{}",
        json!({"type":"user","timestamp":0,"message":{"content":"unanswered typed prompt"}})
    )?;
    for i in 0..3000 {
        writeln!(
            file,
            "{}",
            json!({"type":"user","uuid":format!("r-{i}"),"message":{"content":format!("Another Claude session sent a message:\n<cross-session-message from=\"uds:/tmp/test.sock\" from-name=\"worker\">{{\"result\":\"report {i}: {}\"}}</cross-session-message>\n\nThis came from another Claude session…", "界".repeat(1000))}})
        )?;
    }
    writeln!(
        file,
        "{}",
        json!({"type":"assistant","parentUuid":"r-2999","message":{"content":[{"type":"text","text":"ack"}]}})
    )?;
    drop(file);
    let mut follower = TranscriptFollower::new(&session);
    follower.max_records = 4000;
    let mut adapter = ClaudeAdapter::new();
    let batch = follower.poll_adapter_bounded(&mut adapter, 20, 4096, 256)?;
    assert!(batch.events.len() <= 20);
    assert!(batch.events.iter().map(|e| e.body().len()).sum::<usize>() <= 4096);
    assert!(batch.events.iter().all(|e| !e.body().contains("This came")));
    assert!(batch.events[batch.events.len() - 2]
        .body()
        .starts_with("report 2999:"));
    assert!(
        matches!(batch.events.last(), Some(TalkEvent::ReplyToReport {agent: Some(agent), ..}) if agent == "worker")
    );
    assert_eq!(follower.malformed_lines, 0);
    assert!(follower.unanswered_at(Timestamp(30000)).is_none());
    assert_eq!(follower.unanswered_decision().count, 0);
    Ok(())
}

#[test]
fn large_projected_lines_stream_unused_tool_inputs_and_leave_next_record_intact() -> Result<()> {
    let fixture = Fixture::new();
    let session = fixture.session();
    let records = [
        json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"agent","name":"Agent","input":{"name":"worker","description":"inspect","run_in_background":true,"prompt":"unused ".repeat(150000)}}]}}),
        json!({"type":"user","toolUseResult":{"agentId":"owned"},"message":{"content":[{"type":"tool_result","tool_use_id":"agent","content":"launched"}]}}),
        json!({"type":"user","message":{"content":"typed after large line"}}),
    ];
    let mut file = fs::File::create(&session.transcript_path)?;
    for record in &records {
        writeln!(file, "{record}")?;
    }
    writeln!(
        file,
        "{{\"type\":\"user\",\"padding\":\"{}\", bad json}}",
        "x".repeat(300000)
    )?;
    writeln!(
        file,
        "{}",
        json!({"type":"user","message":{"content":"after malformed large line"}})
    )?;
    drop(file);
    let mut adapter = ClaudeAdapter::new();
    let mut follower = TranscriptFollower::new(&session);
    let batch = follower.poll_adapter_bounded(&mut adapter, 256, 256 * 1024, 16 * 1024)?;
    assert_eq!(batch.events.len(), 2);
    assert_eq!(batch.events[0].body(), "typed after large line");
    assert_eq!(batch.events[1].body(), "after malformed large line");
    assert_eq!(adapter.facts().subagents[0].name, "worker");
    assert_eq!(follower.malformed_lines, 1);
    assert!(follower.poll_adapter(&mut adapter)?.events.is_empty());
    Ok(())
}

#[test]
fn unanswered_record_fixtures_match_raw_and_streamed_decisions() -> Result<()> {
    for case in jsonl_fixture(include_str!("fixtures/unanswered-cases.jsonl")) {
        let name = case["case"].as_str().unwrap();
        let records = case["records"].as_array().unwrap();
        let expected_events = consume(&mut ClaudeAdapter::new(), records);
        let you_count = expected_events
            .iter()
            .filter(|event| matches!(event, TalkEvent::You { .. }))
            .count();
        assert_eq!(
            you_count,
            case["you_count"].as_u64().unwrap() as usize,
            "{name}"
        );
        for batch_size in [1, 1024] {
            let fixture = Fixture::new();
            let session = fixture.session();
            let mut file = fs::File::create(&session.transcript_path)?;
            for record in records {
                writeln!(file, "{record}")?;
            }
            drop(file);
            let mut follower = TranscriptFollower::new(&session);
            follower.max_records = batch_size;
            let mut adapter = ClaudeAdapter::new();
            let mut streamed = Vec::new();
            loop {
                let batch = follower.poll_adapter(&mut adapter)?;
                streamed.extend(batch.events);
                if batch.lines_read < follower.max_records {
                    break;
                }
            }
            assert_eq!(streamed, expected_events, "{name}");
            let decision = follower.unanswered_decision();
            assert_eq!(
                decision.count,
                case["pending_count"].as_u64().unwrap() as usize,
                "{name}"
            );
            assert_eq!(
                decision.oldest.as_ref().and_then(|r| r.uuid.as_deref()),
                case["oldest_uuid"].as_str(),
                "{name}"
            );
            let attention = follower.unanswered_at(Timestamp(0));
            assert_eq!(attention.is_some(), decision.count > 0, "{name}");
            if let Some(record) = &decision.oldest {
                assert_eq!(attention.unwrap().timestamp, record.timestamp, "{name}");
                assert!(
                    !format!("{decision:?}").contains("Fixture"),
                    "diagnostic leaked body: {name}"
                );
                assert!(record
                    .reason
                    .contains("no subsequent assistant text/tool activity"));
            }
            // Replaying into an evicted view must give the same attention decision.
            let mut evicted = TranscriptFollower::new(&session);
            let mut adapter = ClaudeAdapter::new();
            loop {
                let batch = evicted.poll_adapter_bounded(&mut adapter, 0, 0, 0)?;
                assert!(batch.events.is_empty());
                if batch.lines_read < evicted.max_records {
                    break;
                }
            }
            assert_eq!(evicted.unanswered_decision(), decision, "{name}");
            fs::write(&session.transcript_path, "{}\n")?;
            assert!(follower.poll_adapter(&mut ClaudeAdapter::new())?.reset);
            assert_eq!(follower.unanswered_decision().count, 0, "{name}");
        }
    }
    Ok(())
}
