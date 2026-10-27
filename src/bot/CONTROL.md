# Selected response contract
Last verified: 2026-10-26

The source coordinator alone writes `BotResponse.meta.control` and publishes
the response. Primary and local workers never publish executable responses.
The UI FSM alone owns UI phases and final effect permission. The frontend
only renders source/health and never switches a policy.

```json
{"context":{"game":1,"round":2,"event_seq":123},
 "window":{"platform":"majsoul","opened_ns":"-123456789"},
 "source":"native_standby",
 "primary":{"state":"recovering","through":81,"reason":"..."},
 "standby":{"state":"ready","through":83}}
```

- `context` is the original `TrackedEvent.context`, produced synchronously by
  `MonitorHandle::game_event` when the real event is applied. A later monitor
  snapshot only rejects obsolete requests; it never supplies their identity.
- `window` projects the existing bridge-owned monotonic `TimeBudget.opened_at`
  (Majsoul) or `TenhouState.window.opened_at` plus `ops` (Tenhou). `opened_ns` is
  a signed decimal string relative to one process origin. It is opaque and
  comparable only within that process. It is neither wall time nor a new
  clock, epoch producer or UI phase. `None` means no proven input window.
- `WindowProof::capture` and `is_current` are the shared comparison helpers.
  The coordinator captures the current bridge window when admitting the
  original tracked request, then rejects it if that window closes/changes.
  This capture does **not** prove that an ahead-of-tracker bridge update belongs
  to the original request. The UI FSM/integration owner must bind both original
  context and window to its authoritative stage before allowing an effect.
- `source` is `primary`, `native_standby`, or `unavailable`. Selection consumes
  a window once. A late reply can update health, never replace that selection.
- Health is tagged `ready {through}`, `recovering {through,reason}`, or
  `blocked {through,reason}`. `through` counts committed events in the canonical
  current-game journal, **not** global `event_seq` or JAX observation position.
  `ready` says that prefix was consumed. Only the coordinator can certify a
  legal current reply and select primary at a later decision boundary.
- `meta.recovery_timing` contains measured `decision_ms`, actual remaining
  budget at admission, and reserved input milliseconds. These are diagnostics,
  not a second retry clock in the UI.

The response bus remains `BotResponse`; `BotResponse::control()` parses this
typed envelope. The selected identity must travel unchanged through planning
to the final input owner. At final mouse-down the UI FSM must reject stale
context/window, disabled automation, or a different UI stage. It must also
consume its effect once. This patch adds no parallel input controller.

Unavailable is not PASS and is never sent to the executable response bus.
An ordinary `none` can be a considered PASS only when the independent full
legal set contains it for this exact request. Actor, claim target, red tile,
consumed multiset and discard copy identity are validated before selection.
Coordinated riichi requires both tile and `tsumogiri`; no speculative declaration
is fed to either worker, and only the later real platform echo enters the ledger.

Primary service notices do not become real tracker gaps or invalidate a valid
standby answer. A real tracker error expires both sources. No model-health
update or frontend tab change clears that error or grants input permission.
