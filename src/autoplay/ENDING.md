# Ending-screen regression evidence

The 2026-10-26 production journal proves the unknown-frame click path ran.
In `AkagiApp/desktop/data/logs/20261026-154007/all.jsonl`, lines 15154 and
15158 record yellow confirms, line 15162 records a rematch question that
autoplay had not opened, and line 15165 records a successful
`.lq.Lobby.startUnifiedMatch` response (20:23:38.324 America/New_York).
Lines 15168-15215 then contain 20 `tapped where play again comes` messages,
from 20:23:40.686 through 20:24:02.552. The successful request had already
started matchmaking; the old ending plan did not observe that signal.
The earlier `20261026-104100` journal has the same fallback at lines
18485 and 18487, followed by another recognized yellow confirm.

The root cause is explicit in the previous `Acts::end_looked` implementation:
after a yellow confirm, `Sight::Nothing` plus five seconds scheduled
`EndTap`, which clicked `coords::REMATCH` up to twenty times. This treated
absence of a recognized button as permission to press a guessed location.
`press_end` also trusted a previous observation across pointer travel and
hover, and advanced the plan even if input dispatch failed.

No screenshot was saved for each of those journal events. The journal
proves blind clicking and continued clicks after matchmaking; it does not
prove which yellow text was visible in every offending frame. The available
`design/game-end-frames` directory was empty. No OCR or inferred live-page
state is used to fill that evidence gap.

## Implemented behavior

| Current observation | Permitted action |
| --- | --- |
| Blue hand-result confirm | Existing quick-confirm behavior, separate from game-end continuation |
| Yellow end-screen continue/confirm | Click its independently measured yellow face/target |
| Blue rematch beside yellow confirm | Click rematch after reading and rechecking it |
| Yellow/blue rematch question | Confirm only if this sequence actually dispatched rematch |
| Filled-chest reward overlay | After a yellow continue, dismiss at the continue corner; require its gold title and two long panel rules |
| Unknown, transitional, unreadable or disabled-looking frame | Observe again; acquire no click target |
| Successful matching response, including a user request | Cancel the old ending immediately |

The reward overlay has no yellow button in either historical user capture;
its dismissal is therefore logged as `Reward`, never as a recognized
button. It uses the yellow-continue corner, away from the rematch target.
All four ending targets are an explicit match; no default arm gives an
unknown observation the coordinates of a button.

Every ending press remeasures the canvas. After travel, user wait and
hover, immediately before mouse-down it captures again and checks the same
control, unchanged canvas geometry, current autoplay settings, user mouse
activity and the matching-generation ticket. If any check fails, no button
event is sent and the plan reacquires the control. A failed dispatch does
not grant permission to answer the rematch question. After a dispatched
press it observes; a still-visible control can only be retried after a
three-second transition wait and a fresh reading. Existing limits of eight
ending presses and 180 seconds remain; there is no fallback click or prompt
asking the user to take over an unrecognized screen.

## Historical pixels and deterministic coverage

`fixtures/ending/generate.py` extracts only relevant regions from the
existing screenshots. Names, scores and avatars in standings are excluded.
Run it with the design directory as its argument; it requires Pillow and
never contacts the game. Fixtures preserve the source RGB pixels after
normalizing to 960x540, with the unused regions blacked out:

- `continue`, `rank`, `rematch`, `ask`: `game-end-screens/1-standings.jpg`,
  `2-rank.jpg`, `3-chest-rematch.jpg`, `4-rematch-confirm.jpg`.
- `reward-1`, `reward-2`: the two `5-reward-user*.webp` captures, cropped to
  their canvas rectangles `(14,34,1393,784)` and `(0,37,1381,780)` as
  `(x,y,width,height)` before normalization.
- `hand-result`, `table`: `panel-shots/003031-result-hule-8s.png` and
  `003047-table.png`.

The pixel tests exercise those eight fixtures at 640, 960, 1280 and 1920
physical pixels, CSS/device scale factors 1, 1.5 and 2, and nonzero offsets.
The two reward rules are near normalized y=2.75 and y=6.12, with a 0.10-unit
vertical tolerance; 60 of 81 horizontally distributed samples must retain
gold chroma, and the title region must contain at least 15% bright gold.
A missing rule, a faded button, invalid scale or clipped canvas fails.
The yellow face is measured at `(13.75,8.02)-(15.30,8.44)`; the click target
and jitter bounds remain inside the historical button.

Seeded `Acts` tests cover unknown frames after continue, reward dismissal
and reacquisition, real rematch and question, an unowned question, a lost
button, failed dispatch, transition waiting, bounded retries, the last
hand-result boundary, and matchmaking started by either actor. The pure
manager click-plan test rejects changed identity, missing captures and
changed geometry. The bridge test counts only successful matching replies.

Build in the existing shared lock with:

```sh
AKAGI_SOURCE=/mnt/d/Games/Mahjong/dev/Washizu-continue AKAGI_BUILD=continue \
  bash /mnt/d/Games/Mahjong/design/build-core.sh test
AKAGI_SOURCE=/mnt/d/Games/Mahjong/dev/Washizu-continue AKAGI_BUILD=continue \
  bash /mnt/d/Games/Mahjong/design/build-core.sh check
```

The Windows test executable is staged under
`design/staged/core-continue/tests/akagi-tests.exe`. Run the `autoplay::`
filter and
`bridge::majsoul::tests::only_successful_matching_responses_end_the_click_sequence`.
Production was closed before testing. No production installation, data,
bot configuration, weights or live games are used or modified. Integration
and deployment belong to the parent session.

Validation on 2026-10-26: the Windows `autoplay::` filter passed all 273
tests; the successful-matching bridge regression passed; Windows
`--all-targets` checking completed with zero errors and zero warnings.
The named build uses only `target-core-continue` and the existing shared
build lock. Test output is retained in this worktree's ignored
`runtime/continue-autoplay-tests.log` and `runtime/continue-matching-tests.log`;
the compiler check log is `design/check-core-continue.log`.
