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
- Capture keeps the producer-owned window in `ParseResult.window`; conversion
  to `CapturedEvent` attaches it only to the frame's final complete MJAI prefix.
  Historical/intermediate events carry no input window. This includes restore
  backdating and frames whose final event is Dora or another metadata event.
  The tracker adds `context` and `can_act` to produce `TrackedEvent`, without
  sampling the current shared window. The coordinator uses this carried budget
  for its deadline and original proof. Shared slots can only reject obsolete
  requests; they never assign a window to an older event.
- `BotResponse.request: Option<DecisionRequest>` carries the original context
  and concrete producer window internally. It is skipped by serde: bot JSON,
  inspector JSON and UI metadata cannot manufacture input identity. Both the
  direct Native/API/plain-MJAI manager and positioned coordinator attach it in
  the core and produce matching `meta.control`. `WindowProof::capture` is only
  a current-window comparison helper, not an identity producer.
- `source` is `primary`, `native_standby`, or `unavailable`. Selection consumes
  a window once. A late reply can update health, never replace that selection.
- Positioned health is tagged `ready {through}`, `recovering {through,reason}`, or
  `blocked {through,reason}`. `through` counts committed events in the canonical
  current-game journal, **not** global `event_seq` or JAX observation position.
  `ready` says that prefix was consumed. Only the coordinator can certify a
  legal current reply and select primary at a later decision boundary.
- Direct runners report `direct_ready {delivered}` or
  `direct_unavailable {delivered,reason}`. `delivered` counts actual MJAI events
  passed to successful direct response cycles; it does not claim a positioned
  journal receipt or model cache position. Unavailable never reports ready.
- `meta.recovery_timing` contains measured `decision_ms`, actual remaining
  budget at admission, and reserved input milliseconds. These are diagnostics,
  not a second retry clock in the UI.

The response bus remains `BotResponse`. Before response admission the UI
manager drains already-received protocol events in their original order so
queued new-game/round transitions cannot revoke the freshly admitted response.
Planning retains the original `DecisionRequest` and its concrete budget rather
than resampling a new window. The Mahjong Soul FSM token binds both original
context and derived window. After travel, hover and fresh screen capture,
`Environment` reads the monitor's current tracker context (None on a real gap
or ended hand), settings and live producer window immediately before consume.
Mouse-down requires matching original context/window, permitted phase, enabled
automation and an unused effect. Health updates never change a selected source
or revoke a valid standby effect on their own. No additional controller exists.

This final input contract applies to the Mahjong Soul UI FSM. Tenhou carries
the same original envelope through admission/planning and retains its existing
handler/window checks and legitimate riichi continuation; this integration
does not introduce a Tenhou UI FSM or claim its full live qualification.

Unavailable is not PASS and is never sent to the executable response bus.
An ordinary `none` can be a considered PASS only when the independent full
legal set contains it for this exact request. Actor, claim target, red tile,
consumed multiset and discard copy identity are validated before selection.
Executable riichi requires both tile and `tsumogiri`. Direct and coordinated
paths never speculatively feed a declaration or drop its later real echo.

Primary service notices do not become real tracker gaps or invalidate a valid
standby answer. A real tracker error expires both sources. No model-health
update or frontend tab change clears that error or grants input permission.
