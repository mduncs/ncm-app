# ncm-talk

Read-only process/session discovery, incremental Claude/Codex transcript adapters, Talk/Reports
projections, and transcript facts with the `wallcore.py` screen fallback. No hooks, tmux clients,
key sending, GUI windows, or transcript copies are used.

## Integration

`ProcessTree::cached_shared()` shares one `ps -axo pid=,ppid=,comm=` snapshot for two seconds
without cloning every process label per pane. `cached()` remains the owned-copy compatibility API.
`descendants(pid)` includes the root. `harness(pid)` matches executable basenames and ignores
nested harnesses launched as tools. A supervisor with multiple independent harnesses is ambiguous.
`SessionDiscovery::discover(pid)` returns an original `SessionRef`, or `None` for unsupported,
unavailable, or ambiguous sessions. `Default` uses `~/.claude` and `$CODEX_HOME` (else `~/.codex`).
Codex discovery also accepts kernel-owned rollouts under another `sessions` root.

Use `adapter_for(&session)` and `TranscriptFollower::poll_adapter(&mut *adapter)` in the app. This
path deserializes only presentation fields, skipping environment/prompt snapshots, thinking,
images, and tool output except returned agent identities/status. It feeds one projected record
at a time to the adapter. Keep emitted events across polls. The app uses `poll_adapter_bounded` to retain only the newest
256 events/256 KiB per batch and pane, with 16 KiB bodies. `unanswered_at` tracks attention separately
so history eviction cannot dismiss an unanswered prompt. When `batch.reset` is true, clear
the old events; the follower has already reset adapter ancestry/facts. `records_read` and
`lines_read` help schedule a bounded initial catch-up. The existing raw `poll()` contract also
works, but callers should avoid retaining those raw values.

The follower commits byte offsets only after newlines. It skips complete malformed/oversize
lines and increments `malformed_lines`; partial final lines wait without repeated parsing when
file metadata is unchanged. Inode changes, observed shortening, and changed bytes immediately
before the saved offset reset it. This last check covers truncate-and-regrow between polls.
Arbitrary edits earlier in the file that preserve the saved boundary are outside the append-only
transport contract. Default limits are 1,024 lines per poll and 16 MiB per record. Large projected records stream
through the parser after a 256 KiB prefix; ordinary bodies and tool-label fields are clipped before
conversion to adapter values. Tool prompts, patches and file contents are skipped.

Claude follows explicit `turnOrigin` first and compact parent UUID ancestry second. Human
origins, legacy string prompts without `isMeta`, and queued-command attachments produce `You`,
unless a user message has a report content marker. Cross-session prefixes and teammate/cross-session/
agent/task envelopes take precedence over metadata, and their descendant replies stay in Reports.
Report names come from envelope attributes or non-socket `origin.from`; boilerplate is removed and
JSON payloads show result/summary text. Ancestry retains the newest 2,048 memoized nodes.
Queued deliveries redirect subsequent text in the same turn, including an explicit report
turn; duplicates identified by `source_uuid`/UUID are counted once. Sidechains and tool results
produce no Talk messages. Task/peer reports and their replies stay in Reports. Report runs
collapse in `TalkView`, and any later assistant text/tool activity clears `unanswered`, regardless of reply attribution.

Codex uses `response_item` user/assistant messages, ignoring mirrored `event_msg` messages to
avoid duplicates. For historical event-only inputs, explicitly set `CodexAdapter.event_messages`
before feeding records. Codex never emits report events. `turn_context`, token counts, function/
custom-tool calls and their completion update facts. Claude usage context sums input, cache read,
and cache creation; Codex input tokens already include cached input. `facts_at(timestamp)` provides
deterministic subagent runtimes; `facts()` uses the current clock. Background launch tool completion
does not mark its agent idle; finished notifications or completed Codex wait results do. Configure
Claude live facts with `set_live_context(session, started_at)`: launches before the process lifetime
are dropped, and busy requires a current launch plus an own transcript modified within 3 minutes.
Without startup/file evidence the agent is idle. Reset preserves this live context.

`screen_facts(&lines)` strips ANSI and ports SPIN/TOOL/SUB/CHROME behavior. `merge_facts(transcript,
screen)` fills absent fields and lets visible agent rows override matching state/runtime. Shared model types and existing signatures are unchanged.

## Read-only live findings, 2026-10-03

Five Claude processes (versions 2.1.288/2.1.289) had PID JSON metadata whose `sessionId` matched the
newest appended transcript in their project directory. For every process, all other project
transcripts had older modification times. Four matching transcripts began before the process's
`startedAt`, consistent with resumed sessions; the fifth began just after startup. This supports
using the metadata for resumed sessions, but a static observation cannot prove a live `/resume`
switch. Metadata also has `updatedAt`/status fields, so its mtime alone is not evidence of a switch.

No `/clear` marker was retained in those transcripts. One `/resume` system `local_command` record
was present in the active agent transcript, timestamped 2026-10-03T02:03:39.732Z, before the
current process's `startedAt`. This provides explicit resumed-history evidence but cannot prove
that the current PID metadata follows an in-process switch. `/clear` switching remains unverified.
No commands were sent to obtain evidence. These observations do **not** establish that the file is stale.

The implementation rereads the PID metadata on discovery. A uniquely open top-level Claude
transcript from that exact process can override it through libproc. It never selects the newest
project transcript using mtime/`startedAt`: multiple processes can share a project, resumed files
can predate startup, and Claude doesn't usually keep its JSONL open. If a future Claude version
pins metadata and exposes no owned file, safe discovery cannot infer the active replacement;
it retains the metadata session (or returns `None` if absent). A reliable additional ownership
signal would be required for a further fallback.

Real Codex rollouts contained `session_meta`, `turn_context`, `response_item` messages with
`input_text`/`output_text`, function/custom-tool records, `token_usage_record`, and `event_msg`
token counts. The running Codex processes were under a Paseo supervisor pane and each held
multiple rollout files open in the Paseo `CODEX_HOME`. Neither a pane with several harnesses nor
a process with multiple rollouts has a unique session; discovery intentionally returns `None`.
An explicit session id can still be inspected read-only with the example.

Release dump measurements on the shared host (counts only, no bodies retained or saved):

| Input | You | Replies | Reports | Report replies | Malformed | Bytes | Load |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Live Claude pane `%23` | 141 | 365 | 75 | 151 | 0 | 136,941,109 | 214 ms |
| Live Codex rollout (explicit session; supervisor pane `%49` ambiguous) | 10 | 6 | 0 | 0 | 0 | 271,413 | 6 ms |
| Largest Claude transcript | 411 | 935 | 156 | 375 | 0 | 233,131,747 | 424 ms |

Largest-load maximum RSS was 18,186,240 bytes (about 17.3 MiB), measured with `/usr/bin/time -l`.
The full dump of that file, including Talk/Reports/facts with stdout discarded, used 21,921,792
bytes maximum RSS (about 20.9 MiB) and took 0.73 seconds end to end.
These are one-run observations, not guarantees under disk or host contention. The largest file
was selected by size across top-level Claude project JSONLs; it was not copied into the workspace.
`scripts/check.sh` passed (workspace formatting, denied-warning clippy, tests and doc tests), as
did `cargo clippy -p ncm-talk --all-targets -- -D warnings`. ncm-talk now has 21 passing synthetic tests.

## Diagnostic example

Run builds and live transcript loads through the host admission wrapper:

```sh
CARGO_HOME="$PWD/target/cargo-home" cargo build --release -p ncm-talk --example dump
target/release/examples/dump %23
target/release/examples/dump SESSION_ID --counts-only
```

`dump` prints discovery, class counts, Talk blocks, reports grouped by agent and their timeline,
facts, and unanswered state as plain text to stdout. `--pid PID` bypasses tmux when diagnosing
a process directly. `--counts-only` never prints message bodies. No live output is stored in
the repository. Synthetic tests create small files in `target/talk-fixtures` and clean them up.

`ncm --diagnose-talk --pane %40 --pane %45` prints each pane's pending count and the oldest
pending record's timestamp, UUID, type, origin kind and decision reason, without message bodies.
Attention ignores interruptions, compaction summaries, slash/local commands and reports. It
survives presentation eviction, clears on subsequent assistant text or tool activity, and
deduplicates queued source/delivery IDs and queued prompts persisted as merged user records.
