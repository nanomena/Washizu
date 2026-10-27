# The interface

Two views of the same React/TypeScript/Vite frontend: the account chooser
(`?view=profiles`) and the monitor's dock with the game's stage above it.
Both use the monitor's palette, type and controls, without a component library.
The monitor keeps its 1280×960 canvas and `hooks/useCanvasScale.ts`. The chooser
is a compact floating guide with unscaled type, separate from that game canvas.
`routes/Profiles.css` documents its fixed Grid slots and `desktop/src/profile-layout.js`
derives the native size from those slots and two to four visible account rows;
only the profile list scrolls. All its words go through the same `tr`
and language tables as the monitor. The chooser subscribes before reading its
initial state and uses `revision` to discard late snapshots.

The page draws what the core's monitor snapshot says and nothing else. It
reaches the application through `src/lib/host.ts` — commands by name, events by
name — which the desktop shell answers (`desktop/`). It knows the game and the
mjai actions, and nothing about any bot: what a bot wants shown about itself
arrives as words in the snapshot (see "What a bot says about itself" in
`src/bot/README.md`).

An inference/sync fault replaces the fixed decision-status card on every tab.
`sync_error` invalidates the current hand; `sync_fault` keeps the unresolved
fault visible across deals and reconnects. A new deal or a `none` reply is not
proof of recovery: only a fresh actionable reply or displayed model PASS in a valid hand clears the
latched fault. The autoplay setting remains visible beside it.

```text
src/
  main.tsx                 mounts the one page
  index.css                the browser's defaults taken down to nothing
  routes/Monitor.tsx       the page: the stage, the dock and its tabs
  routes/Monitor.css       all of its styling; the canvas is laid out once at 1280 × 960 and scaled
  components/monitor/      the decision card and candidates, the table, tiles, diagnostics
  hooks/useMonitor.ts      the snapshot: fetched once, then followed
  lib/monitor.ts           everything the page derives from a snapshot; no React in it
  lib/tileFace.ts          tile faces, drawn once each and cached
  lib/history.ts           the games on record: rooms, yaku, the figures of the statistics tab
  lib/i18n.ts              the words on screen in the language chosen; lib/i18n/ has a table a language
  lib/host.ts              the bridge to the shell
  types.ts                 the shapes the core sends
```

Two rules the page keeps:

- **Fixed places.** Every region of the dock has a slot of one size. A state
  changes what a slot shows, never where the slots are, and a tile is one size
  whatever the hand holds.
- **The bot's words as written.** Numbers and captions a bot supplies are shown
  as they are; the page neither rescales nor explains them — nor translates
  them.

## Languages

The page is written in Simplified Chinese, and that text is the key: a text on
screen is `tr('对局开始')`, or `tr('另有 {0} 项', n)` with values put in by
position, and `lib/i18n/en.ts`, `ja.ts` and `zh-TW.ts` each give it in their
language. A table of words defined outside any function is wrapped in
`localized(...)` so that it answers in the language of the moment. The choice
is on the settings tab and is kept in the browser's storage; it starts from
the language the browser asks for.

To add a text, write it in Chinese through `tr` and run the tests:
`lib/i18n.test.ts` reads every such text off the sources and fails, naming
them, for any a table lacks (or still has after the page dropped it), and
`Monitor.test.tsx` draws every tab in English and fails on any Chinese left.

Fixed places hold in every language with one set of widths: each cell is as
wide as its widest words, whichever language they are in, and English uses the
short forms riichi players write (E/S/W/N, `r5m`, `Cut`, `Wd`/`Gd`/`Rd`) so its
words fit the same cells. A word and its other state — on / off, a step's name
plain and bold — reserve the wider of the two, so nothing shifts as it changes.

## Commands

```sh
npm ci
npm run build   # type-check, then build into dist/ — what the shell serves
npm test        # vitest
npm run lint
npm run dev     # the page alone in a browser; with no shell it says it has no connection
```
