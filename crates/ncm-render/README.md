# ncm-render

`ThumbnailRenderer::render` keeps the original owned-output interface. Use one instance per card,
or `render_card(window_id, grid, tints, overlays, size)` for a shared atlas and multiple cached cards.
The latter returns a borrowed image and avoids a full RGBA copy on every update. `last_stats()`
reports source rows changed and visible rows painted. Drop closed cards with `remove_card`; use
`clear_cards` after changing the displayed session. The app controls the schedule (currently 1 Hz, within the SPEC 2 Hz maximum). Cached source rows
use fingerprints instead of retaining a duplicate Grid. `Overlays.edge_color` optionally supplies
the tmux bell/activity/silence edge; text overlays still do no image work.

Pictures cover the area inset 4px from the card rectangle, crop equally on the left/right, and keep
the bottom. Per-cell SGR colors override `PaneTint`; reverse/dim, synthetic bold/italic, underline,
wide slots and combining codepoints are supported. Glyph coverage is cached at thumbnail size,
with fractional placement, rather than rendering a native image and scaling it down. The fontdue
atlas does not perform ligature or complex-script shaping. Glyphs are clipped to their source row
band so changed-row repainting cannot erase neighboring rows. A state-edge change restores the
visible picture rows before repainting the edge. Selection changes are terminal cells and leave images untouched.
`Overlays` retains its original fields, but only `state` and `edge_color` affect image pixels. `render_preview` fits the entire window inside its image.

`resolve_font` reads Ghostty's `font-family` entries and resolves installed families from bounded
SFNT name tables in the macOS font directories, preferring a regular face and rejecting other
families. The viewer no longer loads CoreText/CoreFoundation frameworks to discover a file. It falls back to SF Mono Regular, then Menlo. `with_font`
accepts explicit font bytes, including on other platforms. No GUI app or terminal is opened.
`TintResolver::new(snapshot)` spreads muted OKLCH L=0.20/C=0.03 hues across visible projects;
`window_tints(window)` returns pane rectangles. Explicit tmux tint takes priority, then PowerShell/
Windows/pwsh and WSL/Ubuntu labels. Project roots follow wallcore's code/code-storage/research rule.

`KittyEncoder` emits direct kitty APC packets with configurable chunk size. Passthrough and
nested-depth APIs are retired. `encode_placement` supplies c/r explicitly;
`delete` uses d=I to free image data and virtual placements.

Prefer `placeholder_rows` to the legacy `placeholders` Grid: it emits 38;5 for ids 1–255, RGB for
larger 24-bit ids, and clears underline color. The model's RGB-only style cannot represent indexed
foreground, so the Grid API retains RGB addressing. Each first cell has row/column diacritics and
remaining columns are inferred. `placeholder_row` restores fragments anchored within the first
64 columns; restore the entire row from column 0 when a covered fragment starts farther right.
These are ordinary text bytes, with no newline or cursor motion, and must not be wrapped in DCS.
Addressing follows the [Kitty protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/#unicode-placeholders).
`encode_tiled` partitions taller cards into ≤64-row images, reserves consecutive ids, returns each
row offset/transmission/placeholder rows, and preserves every pixel scanline. Keep the ids stable
across retransmissions; remove old tile ids when geometry changes. The encoder only returns bytes:
capability detection and actual terminal writes belong to the app.

`braille_thumbnail` ports deck.py's mini-mode sampling with contain-fit, 2×4 dot masks, any ink,
majority foreground and majority background. Supply resolved pane backgrounds in the input Grid.

Run the file-only PNG/timing harness through the host admission wrapper:

```
CARGO_HOME="$PWD/target/cargo-home" cargo run --release -p ncm-render --example cards
```

Outputs are `target/render-out/fixture-grid.png`, `live-grid.png`, and per-card PNGs. Both grids are
2560×1440, reserving 30px at the bottom, with no labels. `--fixtures-only`, `--live-only`, and
`--session NAME` are supported. Live access uses the current `ncm-tmux` snapshot/capture APIs and
`ncm-term` parser/compositor, filters to the client session (main outside tmux), and excludes the
caller's pane's window. No live server mutations occur. Each card prints its first full-render
latency and changed-row p50/p95 over 100 local synthetic changes to its bottom row. The full
latency for the very first card includes font loading; PNG writes and captures are outside timing.
