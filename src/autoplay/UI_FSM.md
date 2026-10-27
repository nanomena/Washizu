# Mahjong Soul UI control

Design and integration contract (2026-10-26). This is a module specification,
not the project's work-status ledger or a deployment claim.

## Ownership

`ui::Machine` is the single owner of UI phase, observed-control reaction,
transition waits, finite click attempts and phase tickets. `manager` feeds
real protocol facts, current settings, clock events and observations, then
executes effects. `pixels` only reports `Sight`/`Glyph`. `pointer::Hand`
owns travel/hover/hold progress, not UI phase. `budget::TimeBudget` remains
the bridge-owned decision-window identity and time grant. `InputWatch`
remains a protocol observation, not another controller.

The recovery coordinator owns source selection and legality validation.
The UI consumes its selected semantic action; it never chooses a model,
launches inference, or falls back. The shared seam is the recovery module's
public `bot::control::{DecisionControl, WindowProof}` contract, documented
in its `RECOVERY.md`. Its declarations and `BotResponse` accessors are
mirrored here verbatim so this consumer compiles before branch integration.
The coordinator is the only producer of `meta.control`, with the original
`DecisionContext` and exact bridge window captured **before inference**.
Missing/malformed/unavailable/stale provenance fails admission; it cannot
borrow the newest budget. Primary and native standby use the same path.
The UI never reads an asynchronous monitor cursor to manufacture identity.

`Ticket` is UI phase/effect freshness; it is not another request envelope or
`InputGate`. The final boundary checks settings, page/canvas, the same real
window, its server-granted fixed+add lifetime, uplink consumption and pixels.
Config changes increment a UI settings revision (off/on during hover is
observable). Capture/tab/profile changes retire a UI session; bridge auth,
restore and new-round facts retire the old table generation. Manual input
has its own observation revision. Profile switching must call the existing
`AutoplayContext::invalidate_ui()` ownership boundary before rebinding; it
must never infer a profile from a model name. These revisions carry input
ownership only, never primary health, inference deadlines or source policy.

Integration prerequisite: the recovery branch must supply its coordinator
producer together with these shared declarations. The old baseline bot
manager emits unstamped replies, which this UI deliberately refuses. Do not
deploy this branch alone or introduce an unstamped compatibility path.

## State and events

| Phase | Evidence/event | Permitted effect / next phase |
|---|---|---|
| WaitingStart | new session, lobby, matched game not yet dealt | no click; await protocol |
| Table(Waiting) | current round, no operation list | optional recognized auto-win toggle |
| Table(OwnDecision) | NewRound/DealTile/ChiPengGang budget | selected legal semantic action for that exact window |
| Table(Claim) | DiscardTile/AnGangAddGang/BaBei budget | selected legal semantic action for that exact window |
| Result / Transition | real Hora/Ryukyoku; result counter | observe; blue confirm only after reaction and fresh capture |
| Continue | real EndGame plus yellow control | one yellow press; then observe / bounded retry |
| Reward | recognized title and rules, after own continue | one dismissal at continue corner; then observe |
| Rematch | real blue rematch beside yellow control | one rematch press; then await question |
| RematchAsk | question recognized after own successful rematch dispatch | one confirm; await authoritative matching |
| Matching | successful startUnifiedMatch/matchGame response | cancel **all** old result/end effects immediately; no click |
| WaitingStart | successful cancelMatch/cancelQueue/cancelUnifiedMatch/leaveRoom response | retire any pending screen/decision; await a real game/round start |
| Transition / Unknown | absent/unreadable/changed control | observe only; deadline -> WaitingStart, never guess coordinates |
| Suspended | disabled/platform change/manual intervention/capture loss | revoke tickets; require fresh protocol/observation to resume |

Result and ending are one phase domain, never simultaneous `Option`s.
On its first mid-hand subscription, the UI may enter a table phase from
the coordinator's verified current decision/window and legal plan. This
does not manufacture StartGame or revive a retired session/ending.
The final hand's result carries an ending continuation in its payload;
yellow sight ends result control immediately. Unknown is an explicit phase
inside an observation sequence, not permission for another target. A reward
is an overlay, not an invented yellow button. A dispatched rematch is not
proof matching began; only a matching response is.

Settings, manual movement and session changes revoke old tickets. Old
timer/observation/completion callbacks are inert. Resizing invalidates the
planned geometry and requires a new observation. Protocol reentry after
reconnect/new round creates a new phase revision. UI deadlines do not start
a model recovery. Click verification reports evidence; only the machine
authorizes finite retries and requests a client reload when configured.
Each step and retry callback has an ordered ordinal; even refused tickets
are spent. A transport failure after permission counts toward the finite
attempt limit, but cannot claim rematch-question ownership. Successful
matching retires the end plan; it waits without clicks until an authentic
start or cancel/leave response, rather than timing out into a lobby click.
An expired ordinary result returns to table waiting; an expired ending
returns to WaitingStart. Duplicate EndGame cannot restart an expired plan.
WaitingStart retains a typed reason (no game, session change, confirmed
lobby return, ending timeout, press limit, or client reload), included in
phase-transition logs. No stopped plan manufactures a successful transition.

## Migration

| Old owner/field | New owner |
|---|---|
| `Acts.result`, `Acts.end`, `Shown.last`, `Ending.again/asked/continued` | exclusive typed UI state / ending route |
| `Shown.blind_at`, unread -> Burst, burst continuing across changed screens | removed; every confirm needs fresh recognition |
| `Step/EndStep`, `next`, `press_at`, `pending` | one scheduled UI effect and revision-bound ticket |
| `Acts.win_on_at/tenpai_*` | independent auto-win schedule; cannot authorize a phase click |
| `table_acts` branches deciding which ending control is legal | machine transition; executor only checks/executes issued effect |
| `steps.window_checked` only for first click | final check before **every** mouse-down |
| `press.verify_and_retry` authorizing its own retries | bounded retry events through UI machine |
| `Hand` queue / approach / hover timers | remain execution progress; not multiplied into phase states |
| `ManagerState.acted_window` | Tenhou only; Majsoul consumes one machine decision ticket per window |
| `InputWatch.matching` counter | typed `LobbyObservation` sequence and phase; match/cancel/leave are protocol facts |

## Offline qualification

Use explicit monotonic instants and seeded reaction sampling. Exercise
actual guarded mouse dispatch through an isolated transport that records
move/down/up, including final-check rejection and duplicate callback
rejection. Keep eight historical pixel fixtures and real matching-response
parsing tests. Production processes must be absent; builds use
`design/build-env.sh` shared lock, explicit `AKAGI_SOURCE` for this tree,
and only `AKAGI_BUILD=ui-fsm`. Integration/deployment are the parent's job.

## Integrated original request binding

`BotResponse.request` is core-owned and serde-skipped. It carries the original
tracked context and producer window through planning. Queued protocol facts
are applied in order before response admission. `DecisionToken` binds that
context plus the window derived from its original budget; final consume reads
current tracker context (absent on a real gap/end), current producer window,
settings and page facts after capture. A same-window context change therefore
revokes input. Direct Native/API and positioned primary/standby use the same
contract; no UI policy selection or second controller is introduced. Tenhou
retains its prior handler and riichi-continuation behavior. See
[the shared contract](../bot/CONTROL.md).
