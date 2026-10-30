# Platform UI control

Design and integration contract (2026-10-26). This is a module specification,
not the project's work-status ledger or a deployment claim.

## Ownership

`ui::Machine` is the single owner of UI phase, observed-control reaction,
transition waits, finite click attempts and phase tickets. `manager` feeds
real protocol facts, current settings, clock events and observations, then
executes effects. `pixels` only reports `Sight`/`Glyph`. `pointer::PointerController`
owns travel/hover/hold progress, not UI phase. `budget::TimeBudget` remains
the bridge-owned decision-window identity and time grant. `InputWatch`
remains a protocol observation, not another controller.

The machine owns one `effects::Effects<Input>` slot for its scheduled,
issued, permitted or refused effect. That component owns no platform facts,
phase transitions, target selection, retry counts or timers of its own.
Observation callbacks consume only an issued observation; final permission
consumes a press whether allowed or refused; completion retires the effect
before the machine advances or schedules new work. A failed transport after
permission counts as an attempt but cannot claim the next screen. Duplicate
or replaced callbacks cannot cancel newer work, and a reported success
without consumed permission cannot advance a route.

Playing actions no longer belong to this UI machine. The recovery coordinator
owns source selection and legality validation. The shared `TurnController`
consumes its original `DecisionRequest` and `meta.control` proof, and owns
preparation, readiness, pacing, mouse permits, actual command/declaration
confirmation, bounded retries, completion and cancellation. Missing or stale
provenance cannot borrow the newest window. `manager` only joins these objects;
it has no website-specific playing loop. `PacingPolicy` computes one absolute
schedule, and `PointerController` executes every browser mouse effect.

`SiteAdapter` is read-only: semantic action matching, native window/full hand/
control identity, actual CSS bounds, real clock facts and exact-point hit tests.
It cannot wait, retry or dispatch. Local reveal/menu clicks remain in the same
common turn; physical dispatch alone never completes the selected action. A
real declaration echo opens only the validated continuation; full riichi still
requires its selected declaring discard. `transaction.rs` and its shared tests
are the playing-lifetime contract. No decision/retry/finish API remains here.

`Ticket` is UI phase/effect freshness; it is not another request envelope or
`InputGate`. The page-effect boundary checks settings, page/canvas, ownership and observed
pixels or exact Web controls. Playing-window lifetime and command consumption
are checked by TurnController at its own final mouse-down boundary.
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

## Tenhou Web integration boundary

The selected target for this integration is `https://tenhou.net/3/` Web.
Its DOM observations and executable controls must have their own typed
payloads; a Tenhou result or reservation is not a Mahjong Soul pixel `Sight`.
The same machine and effect slot own their scheduling, permission,
completion and bounded retry policy. There is no second click/retry loop.

Client-specific popup identity, popup kind, enabled/closing status, result
queue state, lobby identity, selected rules and reservation availability
must come from the verified Web observer. Wire game/round/end events retain
their own authority. A reservation request is not proof of matching, and a
DOM name alone is not proof of a game/result popup. Web client `1911.js`
uses `name=ok` for popup confirmation and `name=join/cancel` for reservations;
its popup dispatcher decides which queued result or terminal transition
follows. Desktop4K `c6/c8` and its `.click()` dispatch contract do not apply.
`/4/` facts are not a compatibility path for `/3/`.

The Rust DTO is `ui::tenhou_web::Web3Observation`, containing a strictly
parsed `client: "web3"`, nullable `popup` and nullable `lobby`. Popup fields
are `identity`, `tag`, `enabled`, `closing`, `queued`; lobby fields are
`identity`, `lobby`, `offers`, `reservationRequested`; offer fields are
`identity`, `mode`, `enabled`. Missing required facts, wrong clients and
malformed types are unavailable, not empty/default observations. Unknown
popup tags are explicit and have no input permission. Simultaneous popup
and lobby facts cannot acquire a control.

The parent-owned producer's `readUi()` and `probeUi(request)` are read-only,
called through `tenhou::client::observe_tenhou_ui` and `probe_ui_target`.
The probe reports ready, changed or unavailable with an explicit nullable
mouse target. Popup targets use `kind="popup"`, reservations use `"join"`.
An identity must refer to the same actionable client instance, including
its popup type or offer's actual lobby/mode. The producer rechecks enabled,
visible/non-closing status and identity in the same JavaScript evaluation
at the final physical hit point, including occlusion and unchanged geometry.
Hidden-pane buttons cannot be selected; a visible alias must represent the
same real lobby/mode. The bridge exposes no native OK/join effects, DOM click
or synthetic-event effects. It performs no polling, retries or target choice.
The FSM's PressWeb effect goes through the same PointerController::press_when
and CDP move/down/up sequence as play and Mahjong Soul UI confirmations.

| Web phase | Real evidence | Effect / confirmation boundary |
|---|---|---|
| WaitingStart | no known game/player; first lobby | observe; never choose an initial reservation |
| Starting | real player StartGame; TAIKYOKU popup | one permitted popup confirmation; INIT enters Table |
| Table | real INIT | observe Web popup facts alongside the independent selected decision path |
| RoundResult | actual AGARI/RYUUKYOKU popup | one dispatch per popup instance; client advances queued results or sends NEXTREADY |
| FinalResult | actual OWARI popup | one confirmation when auto-continue is enabled |
| WaitingLobby | successful own final confirmation | wait for an actual rebuilt lobby and exactly one enabled offer matching the real table |
| ReservationConfirming | mouse sequence sent | bounded wait for the actual client reservation flag; no repeat; expiry stops |
| ReservationPending | observed client reservation request | no repeat; real StartGame completes the reservation path; an own continuation's queue is bounded (below) |
| Stopped | unavailable producer/dispatch, unknown target, changed/ambiguous/disabled offer, revoked ending ownership, expiry/attempt limit | report typed reason; no guessed replacement room or retry loop |

Only the real StartGame's player seat and `MatchInfo::Tenhou {go_type,lobby}`
establish participant/target proof. Unknown target still permits owned
result confirmation but never a reservation; observer mode has no input
permission. Capture/client generation changes retire both proofs. Settings
and manual ownership changes revoke outstanding effects and retire an
already-dispatched final-to-lobby/queue continuation. A genuine new round
or game explicitly reenters the machine.

Web confirmations do **not** wait for MJAI Hora/EndKyoku. The bridge buffers
multiple wins until real NEXTREADY or INIT; waiting for those model events
before confirming the popup would deadlock. The verified popup is an
independent UI fact and does not manufacture a model event. Delayed MJAI
result/end events cannot restart a Web UI plan.

At the final boundary the manager keeps the same page read lease, reads a
fresh DTO, synchronizes ownership and consumes the exact machine-issued
control/ticket after a fresh DOM hit/identity/geometry check. A sent
popup remains spent for the whole game; its reappearance cannot repeat it.
Changed proves no authorized mouse press and requires a fresh observation and
ticket. Unavailable after permission stops because delivery is unconfirmed.
Observation/guard attempts are bounded by the same machine's phase deadline
and finite limit. Local reservation UI only suppresses duplicates; it is
never represented as successful matching, and cancellation does not requeue.
Typed stop transitions also report their reason through the existing core
notification bus, including target/qualification changes and unconfirmed
delivery. They do not appear only in diagnostic logs.

These are source contracts and offline regressions. Integrated producer,
core compilation and client qualification remain required before deployment.

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

## Tenhou title recovery

The Web3 observation requires explicit nullable `account` and `title` fields.
The account is only the public registered-ID prefix; the credential suffix
never leaves the page. The native title observer exposes the saved-account
button's geometry, and invokes no handler. Guest, registration and ID-entry
controls are not input targets.

Only this FSM learns resume authority: at an authenticated own table, or in
the authenticated lobby while the reservation of its own continuation (a real
game's player, after its own final and reserve dispatch) is confirming or
pending. A reload of the same active game, or of that own queue, can retain the
public identity while revoking every old DOM target and mouse ticket. A title
with the same account may enter `Reconnecting`, independently of
rematching/quick-confirm settings. Fresh clients and their manual
reservations, changed accounts, finished games and changed game identities have
no resume authority. Disabled/manual ownership and final physical hit checks
still revoke input. The common pointer performs the one authorized press.

The reconnect episode has one deadline of 300 s, long enough for a proxy's
failover, and spends each observed control once; its presses are bounded by
that deadline rather than the per-stage press limit. A missing document may be
observed again only inside that bounded FSM episode. A lobby return cannot
authorize a new match. Only authentic `StartKyoku` or `RestoreKyoku` returns
the UI to `Table` and clears the deadline; a click or a disappearing title is
not proof of a restored table. The model source coordinator separately
validates and restores Primary context.

## Stalled own match

A socket can stall without closing: the client then shows no disconnect modal
and waits indefinitely. An own continuation's `ReservationPending` therefore
has a deadline: 15 minutes in the queue, or 60 s after the client's TAIKYOKU
popup announces the match (INIT normally follows in about 12 s). The popup only
bounds the wait and grants no input. On expiry this FSM enters `Reconnecting`
and issues one `ReloadWeb` effect; the executor rechecks its permission
(`consume_reload`) immediately before reloading the page, exactly like the
turn transaction's dead-click reload. Manual or disabled ownership defers the
requested reload; a reload the client cannot start stops with
`DispatchUnavailable`. After the reload the title resume above applies, and
Tenhou's own rejoin returns an ongoing match to the table. A manual queue has
no deadline and is never reloaded.

## Native disconnect confirmation

The native socket-close callback is marked by function identity. The observer
reads only the modal currently owning that same confirmation callback; neither
message text nor a generic OK button grants permission. The callback is returned
unchanged and is never invoked by instrumentation. Unknown/error/login modals
remain unavailable. The Web3 DTO requires an explicit nullable disconnect field.

The same FSM and public saved-account authority permit this modal confirmation
and the following title resume through the shared pointer. Each control is
spent once. A same-game reload retains the original reconnect deadline and
spent-control evidence while revoking all pending targets and tickets. Only a
real round start/restore clears the reconnect episode.
