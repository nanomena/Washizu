# Primary recovery and local standby
Last verified: 2026-10-26

Implemented in the recovery worktree; local offline qualification only.
The minimal integration contract is [CONTROL.md](CONTROL.md).

```text
real post-tracker events + exact window + remaining budget
                     |
              BotManager / Coordinator       sole decision-source owner
                /                 \
   PrimaryWorker (serial I/O)    LocalStandby (embedded NativeBot)
   receipt / full-prefix replay  independent real-event tracker
   positioned replies            local candidates, never cloud
                \                 /
           one validated response per window
                     |
          existing input executor (final window check)
```

The primary worker never publishes actions. The standby never decides
whether to take control. UI only renders typed source and primary health.
Unavailable is not PASS. Advisor remains a separate probability reference.

At a decision boundary the coordinator records `(game, round, event_seq)`
and an absolute inference deadline derived from the actual server window,
less input and scheduling reserve. One canonical confirmed journal belongs
to the coordinator. Both workers observe its ordered real events; only the
primary can read a prefix for replay, and neither worker can append moves. A caught-up, legal primary reply before the deadline wins;
otherwise a valid local candidate wins for this window. Selection is final:
a late primary response only updates recovery progress and is discarded as
an action. Return to primary happens only at a later decision boundary,
after it has consumed the exact real prefix and answered that boundary
legally. No synthetic reach is added to the standby's real event journal.

Primary health transitions:

```text
Ready -- transient error / deadline --> Recovering
Recovering -- replay receipt proves exact prefix --> Ready
Ready + legal reply for a later window --> primary eligible for selection
Recovering -- inference/transport failure --> Recovering (2..30 s backoff)
Ready/Recovering -- inconsistent semantic event --> Blocked
any -- game end / new game --> retire worker and window identity
```

Blocked primary health is independent of standby availability. When both
the primary and a provably legal standby are unavailable, the coordinator
publishes a typed unavailable result and no input action. It does not invent
legal actions, turn unavailability into PASS, or suppress the diagnostic.

The primary pipe has one owner for its entire lifetime. Decision deadlines
cancel only waiting for a positioned result, never a `read_line` future.
Old, duplicate and out-of-order results cannot be consumed as a later
window's answer. Transport restart replays the full ordered game journal,
including earlier hands and the actual moves played while on standby.

Python owns model-state recovery, never source selection. A transaction
retains the last committed tracker/event position, logits and immutable JAX
cache reference. It copies small mutable tracker state, not KV arrays.
Transient inference failure rolls back that transaction for one bounded
retry. Semantic validation remains strict; semantic contradictions
block the primary rather than skipping or rewriting events. Exhausted inference
transactions can restart at later real decisions after 2..30 s backoff; they do
not disable the primary for the whole game. Repeated failures do not cause
per-event retries or an immediate replay loop. Wire positions make
delivery idempotent and prove the server's committed prefix across a
connection replacement.

Public contract (shared with the UI FSM task):

`BotResponse.meta.control` is written only by the coordinator. Its fields
are `context: DecisionContext`, `source: primary | native_standby |
unavailable`, `primary: RunnerHealth`, `standby: RunnerHealth`, and
`window: WindowProof`. Health is a tagged enum, not parallel flags.
The request context is carried by the original `TrackedEvent`. It is
produced synchronously by the tracker/monitor `game_event` application;
the coordinator never substitutes `monitor.snapshot().context` for it.
A later snapshot may only reject an expired request, never identify one.

Three positions have different units and are never interchangeable:

1. `DecisionContext.event_seq` identifies the actual platform event in the
   process-wide tracked stream (game/round tags also travel with it).
2. Primary delivery positions count confirmed events in the current game's
   journal; `(request context, delivered through)` associates a result with
   that exact prefix. Retransmission does not append events or advance it.
3. Python `MahjongPolicyStream.position` counts native model observations.
   Several events can produce zero or several model observations. It is
   checkpointed with the immutable cache, never used as an event receipt.

`WindowProof` must bind the request to the existing bridge-owned input
window (`TimeBudget.opened_at` or `TenhouState.window`), not a new retry or
input controller. It is captured on admission of that original context;
if a request is already superseded it is not admitted. The UI FSM owns
binding this proof to its stage and checking it immediately before input.
The serialization of the existing monotonic window identity is a shared
contract, not a second clock or a new window producer.

The coordinated path accepts complete riichi answers (tile and identity)
from the current Miai and Native runners. It never runs plain-reach
follow-up, never adds guessed `reach` to either journal, and never drops
the later real echo. Timeout, a standby non-reach choice, and a declaration
that was never executed therefore cannot contaminate either real prefix.

Owners:

- `bot/recovery/`: coordinator, primary I/O owner, positioned journal and
  budget/selection state; `bot/manager.rs` is its composition point.
- `bot/recovery/standby.rs`: the local-only standby entry, sharing the native
  engine and action conversion without the cloud route or Advisor lifetime.
- `bot/types.rs`, monitor and frontend monitor types/components: typed
  decision origin and separate primary/standby health.
- The independent UI FSM task owns `autoplay/manager/`, UI phases and final
  execution permission. This task provides the original selected identity,
  never clicks or introduces another input gate.
- `kagami/games/mahjong/mjai_session.py`: atomic model/session commit and
  restore point; no semantic weakening.
- `tools/riichi/akagi/server.py`, `bot/bot.py`: ordered delivery, full-game
  catch-up and explicit recovery status; no standby policy in Python.

Qualification must include real embedded NativeBot, a tiny actual JAX actor,
fault injection across the subprocess/socket/model boundaries, confirmed
standby moves followed by primary catch-up, and final input-window changes.
Constructed mocks certify arbitration and timing only, not end-to-end model
recovery. No production deployment or live account/game operation is part
of this work.

Same-game reconnect requires the provider's identical game id, seat and player
count. The coordinator reconciles the restored current-hand prefix against its
canonical journal and skips those already-applied events in both workers. Only
the new suffix advances them. A missing game id starts a fresh session. A rejoin
that omits an unknown completed hand or contradicts the known prefix is an
explicit gap: neither the model cache nor hidden events are fabricated. A real
tracker gap expires both sources. This boundary remains separate from a primary
model failure, for which an independently valid local standby can continue.

See [qualification](RECOVERY-TESTS.md) for evidence, commands and limits. Final
UI stage/automatic-off/mouse-down qualification belongs to the separate UI FSM
change and the parent's combined regression. This patch does not grant input
permission by itself and has not been deployed.
