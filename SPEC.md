# ncm — spec

`ncm` is one Rust binary that runs inside a terminal (normally directly in Ghostty, outside tmux). It shows
every window of the tmux server as a live, scaled-down picture in a full-screen grid, plus a Quick Look panel
that floats over the grid. Its job: let one person watch 6–16 orchestrator agents and their subagents and
catch them doing dumb things.

tmux stays the source of truth. ncm is a live mirror: it reads through tmux commands and acts through tmux
commands. It owns only temporary grouped viewing sessions, never keeps its own copy of window order, and anything changed in tmux
(reorder, rename, new/closed windows, splits, section moves) shows up within about a second.

## Visual targets (read these first)
- The target screenshots (grid; Quick Look on the Talk and Screen tabs) were captured from md's live tmux
  and are kept private. `reference/final.py` regenerates them from your own tmux.
- `reference/wallcore.py`, `reference/wall8.py`, `reference/final.py`, `reference/talkdata.py` — the
  Python/PIL prototypes that drew those images. They are the behavioral spec for cards, overlays, facts and
  Talk. Port the behavior, not the code structure.
- `reference/deck.py` — the existing text deck (tmux formats, SGR parsing, alert ordering, keys, row-diff
  drawing, braille mini mode). Reuse its tmux format strings and its no-flicker drawing approach.

### Taste rules (hard)
Flat and square. No glow, no shadows, no blur/frost, no rounded corners, no gradients, no fading over time.
Attention never decays: if something needs the user, it keeps looking exactly that urgent until handled.
Hairline card edges `(40,43,51)`; a 2px edge in the state color when the window has a state; selection in terminal cells: a light title bar and light left markers on the two bottom rows. Monospace everywhere.

## Grid
- Full screen; a 30px dock row at the bottom is reserved and never overlaps cards; 8px gaps.
- Column count: the one maximizing `min(card_w / (win_cols*CW), card_h / (win_rows*CH))`.
- Order = the user's status bar order: dashboard sections first (`@dashboard_group` /
  `~/.local/state/tmux/window-groups.json`, if present), then window index. ncm's own window is excluded.
- A card is the whole window — every pane at its real layout with pane borders — drawn at native cell size,
  then scaled to *cover* the card: centered horizontally, top cropped so the bottom (newest output) stays.
  Full bleed: no letterboxing, no separate strips.
- Overlays sit on top of the picture (they take no layout space):
  - top-left, on a dark section-tinted strip: the window's status tab box using the real tmux tab colors and
    text `"{index}{marker} {name}"`, then lowercase harness words (`claude`, `codex`, …) in harness color,
    then env word (`powershell`, `wsl`) when relevant.
  - a pink box `unanswered 3m` when the user's last message to an agent in that window has had no reply for
    more than 20s.
  - top-right: age since `window_activity` with a small square, colored like the status tab background when
    that is bright, else gray.
  - bottom, on a translucent backing `(10,11,14,222)`: the **now line** (tool name amber bold + args, or
    spinner text blue, or plain text) and, only for windows with a harness, the **subagent line**
    (`⤷ N` purple bold, busy names with running time bold white, idle names gray, `+hidden`).
- Dock: `N need you · N working · N done` with state squares on the left, key hints on the right.

## Quick Look
- Space opens/closes it. It floats centered over the grid; the grid stays in place and keeps updating.
- It occupies about 78% of the grid area, with a visible rim of cards and the dock kept clear.
- Header: status tab chip, harness word, tabs `Screen · Talk · Reports N`, facts on the right:
  `model · ctx · N subagents, N busy · active Xm ago`. 2px edge in the state color.
- **Screen**: a fitted live picture of the whole window, using the card compositor and glyph atlas.
  Four-row image strips refresh independently of the one-second card throttle; unchanged strips stay put.
  Talk and Reports remain crisp terminal text within the same panel.
- **Talk**: left column = only the user's messages (pink bar, `you · 14m ago`) and the agent's replies to
  them, oldest→newest, long replies cut to ~9 lines with `… N more lines`. Runs of reports in between collapse
  into one quiet line: `·· 3 reports and 2 replies to them (Reports tab) ··`. A pink bottom banner when the
  last user message has no reply yet. Right column `reports, by agent`: one row per agent, newest first,
  `×count · age`, two lines of its latest report.
- **Reports**: every report and every reply to a report, chronological.
- `[` `]` choose which pane of the window Talk/facts follow (default: the active pane).

## Keys and mouse
| key | grid | Quick Look |
|---|---|---|
| arrows | move selection | ←/→ previous/next window, ↑/↓ scroll |
| Space | open Quick Look | close |
| Tab | next alert (needs → bell → done → activity) | same |
| 1 2 3 | — | Screen / Talk / Reports |
| i | open Quick Look and use the real pane | use the real pane |
| Ctrl-] | passed through | return from use mode |
| Enter | full-screen real tmux outside tmux; legacy select-window inside tmux | same |
| Esc | nothing (never quits) | close |
| q | quit ncm | quit ncm |
| , / x / n | rename / close (asks y/n first) / new window | — |

Click selects a card; clicking the selected card opens Quick Look. A click outside the panel closes it.
Enter, a click inside the panel, or `i` opens full-screen real tmux outside tmux. ncm leaves the
alternate screen, restores termios, pauses its I/O workers, and runs `tmux attach` on a grouped
`_ncm_view_<pid>` session with the selected window. The source session's current window and other
clients remain untouched. Native mouse, scrolling, copy mode (including pbcopy) and colors apply.
The helper status-right says `ncm · C-] back`. Prefix-d detaches, or C-] via the root binding
`if -F '#{m:_ncm_view_*,#{session_name}}' detach-client 'send-keys C-]'` (pass-through elsewhere).
`destroy-unattached` is enabled after attach; ncm also cleans its helper/client on failure or exit.
On return ncm re-enters raw/alternate-screen mode, fully redraws, and re-places/retransmits images.

Legacy `$TMUX` mode uses text/braille only and never probes or emits graphics. Enter and `i`
select the target window/pane in the user's session. No popup or nested client is used.
`--read-only` disables actions. Explicit source sockets support outside-mode attach.

## Data sources — harness-agnostic first
- **tmux**: `list-windows`/`list-panes -a -F …` (formats in `reference/deck.py`) and `capture-pane -p -e`.
  Change detection must keep idle cost near zero and must never capture unchanged panes on every tick
  (e.g. poll one cheap `list-panes -a` with `window_activity`, `history_size`, `cursor_x/y`, `pane_id` and
  capture only panes whose signature changed; capture the selected pane more often).
- Source collection uses the `ncm-tmux` backend interface; control-connection ownership belongs to
  that backend/collector, independently of the explicit full-screen viewing client.
- **Harness per pane**: the user's `@pane_harness` / `@pane_harness_color` pane options when present;
  otherwise walk the process tree from `#{pane_pid}` and match known names (claude, codex, kilo, opencode,
  gemini, aider, …). Every process in a tmux pane also carries `TMUX_PANE=%N` in its environment.
- **Session → transcript, without hooks**:
  - Claude Code keeps `~/.claude/sessions/<pid>.json` = `{pid, sessionId, cwd, …}` for each live process.
    Transcript = `~/.claude/projects/*/<sessionId>.jsonl`; subagents under `<sessionId>/subagents/`.
    Verify whether `sessionId` there changes after `/clear` or `/resume`; if it does not, detect the newest
    transcript in that project dir written by the same process and document the behavior.
  - Codex keeps its rollout file open: find `sessions/YYYY/MM/DD/rollout-*.jsonl` among the open files of
    the codex process or its children (libproc / `lsof`).
  - Anything else: Screen only (no Talk) until an adapter exists.
- **Facts** (now line, subagents, model, context) come from transcripts first (last tool_use name + args,
  `message.model`, usage tokens, spawned agents without a finished notification). Screen-reading with the
  regexes in `reference/wallcore.py` (`facts`, `SUB`, `CHROME`) is only the fallback, because Claude Code's
  screen layout changes between releases.

## Talk classification (verified on real Claude Code transcripts)
- **you**: `type=="user"` with `origin.kind=="human"` (promptSource `typed`, `queued`,
  `suggestion_accepted`); also `type=="attachment"` with `attachment.type=="queued_command"` (typed while
  the agent was busy — counts); also older records with no `origin`, string content, not `isMeta`, not a
  `tool_result`.
- **report**: content takes precedence over origin: a user record starting with `Another Claude
  session sent a message:` or containing `<teammate-message`, `<cross-session-message`,
  `<agent-message`, or `<task-notification>` starts a report turn. Metadata task/peer origins also
  count. Names come from `teammate_id`, `from-name`, agent-message `from`, or a non-socket
  `origin.from`; task notifications also use the Agent summary. Strip the trailing cross-session
  boilerplate and display JSON `result`/summary text. Replies descending from a report turn remain
  in Reports even if `turnOrigin` says human; a queued typed prompt still redirects its turn.
- **replies**: assistant `text` blocks, skipping `isSidechain`. A reply belongs to the turn that started it:
  use the record's `turnOrigin` (`human` / `task_notification` / `peer`) when present, else walk
  `parentUuid` back to the turn's first user record. Turn started by the user → Talk; otherwise → Reports.
  A queued user message that lands mid-turn makes the assistant text after it in that turn a reply to you.
- **unanswered**: the oldest pending human message (queued included), cleared by any later assistant
  text/tool activity in the session. Ignore interruptions, compaction summaries, slash/local commands
  and reports; deduplicate queued prompts against delivered or merged user records.
- Follow transcripts incrementally (keep a byte offset); never re-read whole files each tick.
- Test with small synthetic fixtures written by hand. Never copy real transcripts into the repo.

## Colors — reuse what the user already has
| element | source |
|---|---|
| status tab chip | tmux `E:@status_style_window` / `E:@status_marker_window` when present, else the window-status style |
| state edges/flags | `@agent_state` (needs/done/working) + tmux bell/activity/silence flags. Defaults: needs `#ff2e88`, done `#7ee787`, bell `#d7263d`, activity `#fff176`, live `#0f766e`, working `(76,201,240)` |
| card background | each pane's own tint (`#{window-style}` set per pane); untinted panes get a muted OKLCH tint per project (L≈0.20, C≈0.03, hues spread over visible projects); Windows/PowerShell panes `(1,36,86)`, WSL/Ubuntu `(48,10,36)` |
| harness word | `@pane_harness_color`, else built-ins (claude `(217,119,87)` etc.) |
| section strip | PINNED/BUILD/EXPLORE/PERSONAL hues 250/150/300/70, very faint |
| Talk | you `(255,120,170)`, the agent its harness color, reports `(176,150,240)` |

The README must have a **Tints & colors** section: which colors come from standard tmux (per-pane
`window-style`, which any tmux ≥ 3.0 has — the user's `tmux-window-tone` script sets it with `set -p`) and
which come from the user's own extras (`@agent_state` from agent-alert hooks, `@status_*` tab options,
`@pane_harness` from tmux-harness-scan, window-groups.json). ncm must work without any of the extras, just
plainer.

## Rendering and performance
- Thumbnails: rasterize each glyph once into an atlas **at thumbnail size** (no full-size-then-downscale),
  blit per cell over per-cell backgrounds. Font: Ghostty's configured `font-family` when resolvable, else SF
  Mono (`/System/Applications/Utilities/Terminal.app/Contents/Resources/Fonts/SF-Mono-*.otf`), else Menlo.
- Re-render only changed rows of changed panes; at most 2 thumbnail updates per second per card.
- Kitty graphics protocol with **Unicode placeholders** (U+10EEEE + row/column diacritics, image id in the
  foreground color), sent as direct kitty APC bytes outside tmux. Transmit
  raw RGBA, zlib-compressed and chunked (`f=32,o=z,m=1`). Update an image by re-transmitting the same id;
  the placeholder cells stay unchanged between updates. Quick Look text written over
  placeholder cells simply hides that part of the grid.
- Terminal output: alternate screen, synchronized output (`CSI ? 2026 h/l`), row-diff writes only, never a
  full clear → nothing flashes.
- If kitty graphics are unavailable (one startup query with `a=q`, five-second timeout), fall back to
  braille/text thumbnails like `deck.py` mini mode and say why in the dock.
- Budgets: idle ≈ 0% CPU; 5 busy agents ≤ 3% of one core; RSS ≤ 80 MB; image traffic ≤ 600 KB/s; first
  frame < 500 ms.

## Safety
- Never send keys, kill, rename, resize or select anything in tmux except on an explicit user action;
  closing a window asks first.
- Tests run against a private tmux server (`tmux -L ncm-test-<pid>`, `TMUX_TMPDIR` inside the worktree).
  Only read-only commands may touch the user's live server.
- Each viewer runs their own `ncm` process; it works over SSH into the Mac mini with per-terminal
  capability detection.

## Workspace layout
| crate | owns |
|---|---|
| `ncm-model` | shared types: windows, panes, cells, colors, states, harness, facts, talk events |
| `ncm-tmux` | tmux queries, capture, change detection, actions (select, send-keys, rename, kill, menu) |
| `ncm-term` | parse `capture-pane -e` lines into cells (SGR, wide chars, tints); composite a window's panes + borders |
| `ncm-render` | glyph atlas, thumbnails, overlays, kitty encoder + placeholders, braille fallback |
| `ncm-talk` | session discovery, Claude/Codex transcript adapters, Talk/Reports/unanswered, transcript facts |
| `ncm` (bin) | app loop, grid layout, Quick Look, keys, mouse, dock |

## Decisions after phase 1 (these override anything above that conflicts)
- **Outside tmux (2026-10-04):** Ghostty displays only ncm; the agents' tmux server runs in the
  background. Direct kitty bytes replace all passthrough/re-probe/multi-client image paths. One
  generous startup probe supports SSH; no support means braille/text. Full-screen access attaches
  a real client to an owned grouped session and restores the complete ncm screen/images on return.
  `ssh -t host ~/.local/bin/ncm` needs no interactive-shell PATH: resolve tmux at
  `~/.local/bin/tmux`, `/opt/homebrew/bin/tmux`, then PATH. `--socket`/`NCM_TMUX_SOCKET`
  and default-server selection work in both modes.
- **Card text overlays are terminal text, not pixels.** The card's top row (tab chip, harness/env words,
  `unanswered` box, age) and bottom two rows (now line, subagent line) are written as ordinary text cells
  over the card's placeholder cells, with a solid dark backing `(10,11,14)`. They are crisp, use Ghostty's
  own font, and changing them never re-sends an image. The thumbnail image holds only the picture and the
  hairline/state edges. Selection is terminal cells and causes no uploads.
- **Gaps are inside the image.** Card cell rectangles tile the screen; each card image insets its picture
  by 4px on every side (filled with the page background), giving 8px visual gaps on a cell grid.
- **Glyphs are rasterized at thumbnail size;** native cell geometry is used only for cover-crop math.
- **Placeholder columns use inference:** first cell of each row carries row + column diacritics, the rest
  of the row is bare U+10EEEE (the kitty spec infers column = left neighbor + 1). This keeps us inside the
  first 64 diacritics; cards taller than 64 rows are split into stacked images. Use 256-color foreground
  (`38;5;N`) for image ids 1–255 when possible; it survives every tmux color mode.
- **Which session:** the session of the client ncm runs in (`display -p '#{client_session}'` /
  `$TMUX`), or `--session NAME`, default `main` outside tmux. Windows deduplicated by window id, in that
  session's order. ncm's own window is excluded.
- **Change detection guarantee:** cheap signature polls every 250ms until eight consecutive full
  polls have no signature changes, then every 1s until a change. Only the pane shown in Quick Look
  gets a 250ms capture guarantee. Other panes remain staggered through a round robin at least
  every 3s, independent of metadata backoff. Hash captures and skip parse/render when unchanged.
  Geometry changes invalidate composites.
- **Live Claude subagents:** drop launches older than the process metadata's `startedAt`. A
  background agent is busy only with a launch in this process, no finished notification, and its
  own `agent-*.jsonl` appended within 3 minutes. Missing lifetime/file evidence means idle. Visible
  screen agent rows override transcript state and runtime for the names they list.
- **Bounded catch-up:** stream all records for classification/facts but retain only the latest
  256 events / 256 KiB of display text per pane, clipping individual bodies to 16 KiB. Retain at
  most 2,048 memoized ancestry nodes; unanswered attention state survives text eviction.
- **Process tree lives in `ncm-talk`** (session discovery needs it). `ncm-tmux` determines harness from
  `@pane_harness` or `#{pane_current_command}` only; the app merges in discovery results.
- **Screen-reading fallback for facts lives in `ncm-talk`** (`screen_facts(lines)`), ported from
  `reference/wallcore.py`.
- **Malformed transcript lines:** skip complete malformed lines with a counter; wait on a partial last line.
- **`ncm-model` is shared:** prefer not to change it; if you must, make additive changes only and list
  them in your report.

- **Instant input and real-pane use mode (2026-10-03):** bounded background workers collect tmux and
  transcript data and prepare images. The UI drains pending keys before a row-diff paint, coalescing held
  arrows. Quick Look occupies ~78% of the grid and Screen is a fitted atlas picture in changed four-row
  strips, sharing a 1.4 MB/s paced budget while open. Clicking in or Enter attaches full-screen
  real tmux on the same terminal, with an isolated grouped session and native client behavior.

- **Tty lifecycle and hidden viewers (2026-10-04):** EOF/EIO/hangup and SIGHUP/SIGTERM stop the viewer,
  including during input floods or output backpressure. A bounded input reader isolates crossterm EOF
  behavior; a bounded output worker prioritizes coalesced text frames between image packets and uses
  interruptible nonblocking tty writes. Hidden viewers suspend captures and transcripts, making only
  a ~1s viewer-server visibility query; return resets capture scheduling and catches up promptly.
  Test harnesses own and reap every viewer, including failed runs.
