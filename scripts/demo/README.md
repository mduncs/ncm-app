# Screenshot world

From the repository root, run `scripts/demo/up.sh`. It prints the exact `env … ncm
--socket … --session demo` command to paste into Ghostty, and saves it in
`target/demo/run.txt`. A 200×55 Ghostty viewport matches the fixture geometry.
Run `scripts/demo/down.sh` when finished. Both commands are idempotent; `down.sh`
leaves the transcripts and verification artifacts available for review.

To verify a particular existing ncm build, pass `up.sh --binary /path/to/ncm`
or set `NCM_DEMO_BINARY`; startup snapshots the executable into `target/demo/bin/ncm`.
The printed Ghostty command uses the `ncm` on your PATH. There is no build,
dependency installation, network request, or real agent launch in this demo.

Everything generated is under `target/demo/`: fake HOME, Claude root, empty Codex
root, XDG state, tmux socket, workload PID records, executable snapshots, transcripts,
and screen dumps. The server uses `tmux -L ncm-demo -f /dev/null` with its own
`TMUX_TMPDIR`, session `demo`, `mouse on`, fixed 200×55 windows, and disabled
automatic renaming. It reads no user tmux configuration. A random ownership marker
and a lifecycle lock prevent teardown of an unowned server and concurrent starts.
Symlinked output directories or files are rejected. The default server and the
real harness/state directories are never queried.

The four section colors are read from
`target/demo/home/.local/state/tmux/window-groups.json`, using the real version-1
schema and stable `@dashboard_key` values. No `@dashboard_group` overrides hide
whether the file is working. ncm currently tints card headers rather than drawing
separate section headings.

| Window | Section | Demonstrates |
| --- | --- | --- |
| otter-ledger | pinned | Busy Claude, 1-second tool/spinner updates, Talk and Reports |
| plum-forge | pinned | Busy Claude, 1.7-second updates, small fictional diff |
| velvet-radar | pinned | Needs you; prompt aged 3 minutes, pink card badge and unanswered Talk banner |
| moss-kettle | build | Busy Claude, 2.4-second updates, fictional test run |
| amber-orbit | build | Done Claude, quiet regression summary |
| pebble-vault | build | Done Claude, quiet patch review |
| lilac-engine | explore | Busy fixture with Codex badge, 3-second updates, subagent/tool facts |
| waffle-comet | explore | Idle Claude, quiet conversation and reports |
| noodle-beacon | explore | Scrolling fake build/test log; no build is executed |
| mint-canyon | personal | Colorful fictional queue monitor; no host process inspection |
| coral-picnic | personal | Plain interactive `/bin/sh` in the throwaway HOME |
| cloud-teapot | personal | Two side-by-side panes with independent fictional streams |

Every Claude fixture has two human/reply turns, a consecutive run of three
task-notification reports from `fern-scout`, `glaze-checker`, and `sprout-builder`,
long replies that fold into “more lines”, a model, context-token usage, and subagent
facts. Seven panes have `@pane_harness claude`; `lilac-engine` has
`@pane_harness codex` to demonstrate the green harness badge. All eight have realuse-style
`claude/sessions/<pid>.json` records whose `tmux` mapping identifies the exact pane.
Workloads run through `scripts/realuse/fake_agent.py --own`, which registers PIDs
and terminates/reaps its child on shutdown. Tool names, commands, and diffs are
printed strings. The dock reads **1 need you / 4 working / 2 done**.

The pane badge override is independent of session discovery. The Codex-labelled
pane intentionally uses the same fake Claude-format transcript and PID mapping
as the others, so all agents can demonstrate Reports. This exercises mixed visual
labels, not the native Codex rollout protocol: ncm's Codex adapter does not emit
task-notification Report events. No real harness runs and no ncm source changes
were made.

Click `otter-ledger` (or select it and press Space), then `2` for Talk or `3` for
Reports. Press Esc, open `velvet-radar`, and press `2` for the unanswered banner.
Right-click any card for the menu. All user-visible paths are fictional
`~/work/<whimsical-name>` paths. The plain shell is real; commands typed there run
normally, so keep it untouched for screenshots.

## Offscreen verification

`scripts/demo/verify.sh` uses the realuse pyte terminal and leaves the world stopped.
It needs the already-installed packages in `scripts/realuse/requirements.txt`;
it deliberately does not install anything or contact a package server. It uses
`target/realuse/venv/bin/python` when available, or `python3`. Select another
existing Python with `NCM_DEMO_PYTHON`.

The verifier checks all eight `--diagnose-talk` mappings, all twelve window names,
geometry and mouse mode, mixed harness badges, four header tint groups, dock counts, Talk messages and
replies, folded long messages, collapsed reports, the reports-by-agent column,
Reports contents, model/context/subagent facts, the unanswered badge/banner, and
the right-click menu. It also tests repeated up/down and refusal to tear down a
server with a foreign ownership marker.

Artifacts are in `target/demo/`:

- `verification.md`, `verification.json`: checks and results.
- `diagnose-talk.txt`: discovery evidence for every pane.
- `windows.txt`, `section-colors.json`: geometry and visible tint evidence.
- `screens/01-grid.txt`, `02-screen.txt`, `03-talk.txt`, `04-reports.txt`,
  `05-unanswered.txt`, `06-menu.txt`: readable screen dumps.
- `screens/*-raw.txt`, `screens/*-styles.json`: original pyte text and cell colors.
- `screens/pane-*.txt`: individual tmux workload captures, including both split panes.
- `demo.vt`, `demo-terminal.json`: VT bytes, exact PTY inputs, and image hashes.
- `cleanup.json`: zero live owned PIDs, no private server.

The readable dumps replace Kitty image placeholder cells with spaces. pyte
verifies text, styles, and protocol activity, not raster pixels; the lead's real
Ghostty screenshot is the visual image check. No GUI windows or audio are used.
