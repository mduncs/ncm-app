# ncm — parked 2026-10-04

md paused the project. This note covers where it stands and what would make it better.

## State

- **Last good commit:** `2d07c99`, the outside-tmux build.
  - Ghostty runs only `ncm`; tmux stays in the background.
  - Real-use harness: 72/0, twice. `scripts/check.sh` passes.
- **To run it:**
  - Mac: a plain Ghostty window running `ncm`.
  - Laptop: `ssh -t host ncm` in its own Ghostty tab, not inside the laptop's tmux.
- **Window `main:1`** still runs the older in-tmux build (17:28). Quit it with `q` whenever.
- **Inside tmux**, the current build falls back to text/braille only.

## Checked by eye in a real Ghostty

Works:
- live pictures;
- one-click preview;
- Esc, or a click outside the preview, closes it;
- Enter, or a click inside the preview, opens real full-screen tmux;
- C-] or prefix-d returns and the pictures come back;
- `n`, resize and `q`;
- about 1% CPU.

Broken:
1. **Scan lines.** Busy cards repaint in bands, because strips upload one at a time under a byte budget left over from the tmux-passthrough days.
2. **Wrong slices beside overlays.** Next to the preview or the menu, card edges show the wrong part of the picture.
3. **Right-click menu.** It opens mid-screen, not at the click, and doesn't name its window.
4. **Return state.** Grid → Enter → C-] returns to the preview, not the grid.
5. **Status bar name.** Full screen shows `_ncm_view_<pid>` where md expects `main`.

## In flight when parked

Worker `ncm-overlay` (brief `briefs/20-overlay-clip.md`, plus two follow-up messages) is fixing all five. It had not finished when the project was parked.
- When it finishes, its patch is parked at `.workers/ncm-overlay.patch`. It is not applied or verified.
- To resume: run `workers apply ncm-overlay`, then `scripts/realuse/run.sh` twice, then `scripts/check.sh`. After that, check by eye on md's real, busy windows.

## Making it fast and smooth (the plan given to ncm-overlay)

| Piece | Effect |
|---|---|
| Redraw only panes that control mode reports as changed (`%output`) | A quiet agent costs nothing |
| Send only changed tiles, zlib-compressed | Idle ≈ 0 bytes; three new lines → a tiny patch (matters over ssh) |
| Atomic commit: upload changed tiles under fresh ids, then switch all of a card's cells in one paint, then delete the old ids | No scan lines, never a mixed frame |
| About 1 frame/s per card, faster for the selected card and the preview; nothing while hidden or backlogged | Bounded cost at 16 agents |

The only faster option is a native Mac app drawing live terminals on the GPU (e.g. libghostty). That would be a new project, and it wouldn't work over `ssh -t host ncm`.

## Open design question: everything vs only what's active

Today ncm shows every window in the session as an equal card, so 16 windows means 16 small cards. md wasn't sure how a workspace cleanup would change this. Options:
- **Active-first:** show windows with recent output or an unanswered question as cards; collapse quiet ones into a one-line strip at the bottom; click the strip to expand.
- **Agents-only:** show only windows running a harness (claude, codex and so on); shells and tools get hidden.
- **Per-workspace:** one ncm per tmux session (`--session`). After a cleanup into a few sessions, each wall stays small.

Pick after the cleanup, once it's clear how many windows are actually live at once.

## Lessons

- The harness's fake terminal can't see pictures. Anything visual still needs a real-Ghostty check, on busy panes, not quiet test windows.
- A fully covered Ghostty window stops repainting. Let a sliver show past md's window edge when screenshotting in the background.
