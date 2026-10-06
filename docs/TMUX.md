# tmux source and terminal-grid integration

The existing public function signatures and `WindowInfo`/`PaneInfo` field sets are unchanged.
The source uses one `tmux -N -C attach-session -f ignore-size` client, with pipes and
no terminal negotiation, helper session, selection changes, or option writes. Only explicit action
methods mutate panes/windows. Tests mutate private named servers. As with any attachment, tmux
tracks the observer as an attached client and may run configured attach/detach hooks.

## Session and metadata

`Tmux::socket(path)` implements `--socket` / `NCM_TMUX_SOCKET` with `tmux -S`; it ignores inherited
`TMUX`, `TMUX_PANE` and session ids and defaults to `main` without a session override.
`Tmux::with_session(name)` implements the CLI override. Otherwise `current_session()` resolves
the session id in the inherited `$TMUX`, with a read-only `#{client_session}`/`#{session_name}`
query fallback; outside tmux it returns `main`. Named private-server handles ignore the inherited
server and pane. `testing::PrivateServer` pins its handle to `ncm-test`.

`snapshot()`/`poll()` query panes in that session, deduplicate stable window and pane ids, and
exclude the live viewer's entire containing window when `TMUX_PANE` is set. Linked windows use
the chosen session's first (lowest) index. No captures or process-tree scans occur in a snapshot. The poll targets `$TMUX`'s session id
or the explicit session directly, issuing `list-panes` over the shared control connection.
`WindowFlags::zoomed()` reads `Z` from the existing raw flag string, preserving struct literals.

Harness overrides come from `@pane_harness`/`@pane_harness_color`; absent a name, known command
names and paths in `pane_current_command` identify claude, codex, kilo, opencode, gemini, aider,
pi, prime and fable. A command containing node plus a codex entry point identifies codex; node
alone cannot identify a harness. Process-tree discovery belongs to ncm-talk and can refine this.
`env_word(label, raw_title)` returns `ps` for powershell/windows, else `wsl` for ubuntu/wsl, else
empty. `PaneInfo::env_word()` uses the displayed title; `Tmux::pane_env_word(id)` queries both
label and raw title when a custom label could hide the environment.

Tab styles parse expanded `#{E:@status_style_window}` with standard window-status-style fallback,
including chained `#[fg=…]#[bg=…]#[bold]` fragments, default resets and nobold. Markers use the
expanded option; an option whose expression expands empty stays empty. When absent, the state
marker is used. Named/bright colors, colourN/colorN and #rrggbb use the deck's One Half Dark
base-16 palette and xterm cube/grayscale. Per-pane window-style provides the tint.

## Group-file schema and ordering

Read-only inspection of `~/.local/state/tmux/window-groups.json` and `~/.local/bin/tmux-group-state`
found schema version 1. The state is keyed by session name, then the UUID stored in each window's
`@dashboard_key`, **not** the volatile tmux window id or index:

```json
{
  "version": 1,
  "sessions": {
    "main": {
      "updated_at": 1700000000,
      "windows": {
        "synthetic-uuid": {
          "group": "build",
          "name": "synthetic-project",
          "path": "/synthetic/project",
          "panes": 2,
          "last_seen": 1700000000
        }
      }
    }
  }
}
```

The two timestamps are epoch seconds. `path` is the active pane's current path; `panes` is the
window's pane count. The actual status helper uses `TMUX_GROUP_STATE_FILE`, then
`XDG_STATE_HOME/tmux/window-groups.json`, then `~/.local/state/tmux/window-groups.json`; ncm honors
the same paths. Missing, unreadable, malformed or unsupported-version files supply no fallback.

A nonempty live `@dashboard_group` wins. For a missing group, use the UUID's saved record first.
Recovery without a usable UUID matches `(name, active-pane path, pane count)`, then `(name, path)`,
then a unique path. For multiple exact/loose matches, choose greatest `last_seen`, matching the
helper; reserve records already owned by another window. ncm never creates UUIDs or reclassifies
windows by guessed names. Only the four current saved groups are valid recovery records.

`~/.tmux-main-status.conf` has overview row 0, then rows 1–4: **pinned, build, explore, personal**.
Each section's `#{W:…}` iterates windows in ascending session window index. The called helpers
`tmux-group-state`, `tmux-window-tone` and `tmux-harness-scan` update group/tint/harness metadata;
`tmux-group-drag` changes group or tmux window index on explicit mouse actions. None supplies a
separate hidden sort order. ncm sorts those four sections first, ascending index within each,
then unknown groups, then ungrouped windows. It keeps no private window ordering.

## Scheduler and composition

Call `CaptureScheduler::tick(&tmux, selected_pane_id, Instant::now())` on each collector tick.
A `ScheduledPoll` owns reusable metadata and a `CapturePlan`: capture `plan.panes`, invalidate
`plan.geometry_changed`, and refresh structure when `plan.structure_changed` is true.
Metadata initially refreshes every 250ms and backs off to 1s after eight unchanged ticks. Window,
layout, session, focus, and rename notifications request a fresh snapshot. The periodic metadata
query retains support for custom options, tint/group files, titles, and other unnotified formats.

`%output` and `%extended-output` mark the emitting pane dirty. The reader retains at most 256 bytes
of notification header, discards the payload without decoding or queuing it, and handles arbitrary
buffer fragmentation/non-UTF-8 output. Raw notifications stay enabled: `no-output` would lose the
pane change signal. Command responses are framed by matching `%begin`/`%end`/`%error` guards;
notification-looking captured cells are ordinary data inside a response. EOF reconnects on the
next query and invalidates the scheduler's content state. Drop kills/reaps the exact client and
joins its reader; collector shutdown is also joined.

Read-only behavior is enforced by the observer transport's command allowlist: only list/show queries,
`display-message -p`, and `capture-pane -p` can reach its stdin. Explicit actions use ordinary command
clients and remain disabled by `--read-only`. The tmux `read-only` client flag is intentionally absent:
on tmux 3.7 it makes external `send-keys` fail if tmux chooses the observer as that command's implicit
client. Keeping the observer size-neutral and restricting its commands preserves external input.

First sightings capture immediately. Ordinary cards capture only dirty panes, retaining pending
changes across their shared one-second cooldown. Quick Look retains 250ms captures of its window.
Pure `schedule(&poll, selected, now)` remains available for callers without control notifications;
it compares activity/history/cursor/size signatures and retains the staggered 15s safety refresh.
Removed panes are evicted. Geometry and active-pane changes invalidate composites.
`retry(ids, now)` retries failed captures. Capture/display/list commands reuse the same connection.
Viewer visibility and graphics queries share it when source and viewer are on the same server;
a remote source needs a separate observer on the viewer server. Control clients are excluded from
the set of real viewing terminals and from use/menu client selection.

`CaptureHashes::changed(&captures)` returns only content changes, so forced unchanged captures
need no parse/render; hashes retain panes absent from incremental capture batches. Use
`CaptureHashes::retain(&poll)` after removals. Metadata changes (including tint/title/state) still
need app updates even if content hashes are unchanged.

`Tmux::pane_border_styles(window_id)` reads inherited pane-border-style and pane-active-border-style
without changing options. Use those with `compose_window_with_styles(window, pane_grids, &styles)`;
cache the styles between metadata refreshes. The existing `compose_window` uses the specified
(60,64,74) fallback. `BorderStyles` is an additive model type with normal and active CellStyle
fields; PaneInfo and WindowInfo retain their field sets. Border junctions derive from real pane
coordinates; borders touching the active pane take the active style. In a zoomed window only
the active pane is composed, so hidden pane rectangles cannot overwrite it. Capture parsing keeps reverse
as an attribute for the renderer to apply after resolving default foreground/background colors.

Actions include `select_window_pane(window_id, pane_id)` for session-aware linked-window selection,
plus the existing `select(pane_id)`, literal text, named keys, rename, kill, detached new-window,
and display-menu methods. The caller must confirm kill and invoke actions only on explicit input.
Literal arguments escape tmux's own trailing-semicolon command parser in addition to bypassing
shell interpolation. Display-menu requires an existing client; detached test servers have none.

## Live measurements

Run through the host admission wrapper:

```sh
CARGO_HOME="$PWD/target/cargo-home" \
  cargo run -p ncm-tmux --example live_costs -- main
```

The example reports snapshot milliseconds, one batch capture-all milliseconds and visible pane
count, then 10 seconds of idle scheduling with no selected pane. Initial full capture is outside
the scheduler count. The live server can still produce output; `changed_captures` distinguishes
actual content changes from unchanged refreshes. These timings measure the source, not rendering.
Measurement output is saved in `target/live-costs.txt` for this worker's run.

Worker measurement on 2026-10-03, live `main`: 29 panes, snapshot **53.754ms**, capture-all
**125.924ms**; 10.439s scheduler run, 36 polls, **150 captures**, **51 content changes**, total
capture time 2337.417ms. The server was producing output, so this is a live idle-scheduling run,
not an assertion that the server was idle. These costs are above the earlier SPEC probe's ~16ms
capture-all figure; host/server contention and tmux response latency remain relevant. Refresh
bounds assume timely app ticks and successful/serviced tmux queries; blocked I/O cannot satisfy
a real-time bound by itself. No rendering, GUI, audio or live mutation was performed.
