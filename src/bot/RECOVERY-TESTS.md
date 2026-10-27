# Offline recovery qualification
Last verified: 2026-10-26

This is a development-worktree qualification. No application/account was
opened, no live table was operated, and no installed configuration, runtime
checkout or production weight was changed. The matching Python change lives
in the `nanogamekits-recovery` worktree.
Matching Python commit: `62f3370d84752b3ee2cf8c1d14abc42e87201aa7`.

| Boundary | Evidence | Result |
|---|---|---|
| Core regression | Windows library test executable, four threads | 859 passed; two environment-dependent tests run separately |
| Cross-process timeout | Actual Python relay, production socket/delivery/session, tiny JAX actor, embedded NativeBot | Standby owns first window; late primary cannot replace it; primary consumes the actual standby discard and returns at the next legal window |
| Cross-process model failure | Same real chain, model executes then throws twice | Service rolls back both transactions; local action continues; fresh full-prefix session recovers after backoff and returns a validated action |
| Prefix identity | Exact delivery receipts, duplicate/out-of-order replies, canonical journal duplicate/rejoin tests | No double application or stale action publication |
| Decision timing | 3,000 ms real test window, 600 ms input reserve | Native selected around 2,400 ms; healthy next primary around 15 ms; recovered primary around 194 ms in local CPU runs |
| Local legality | Real embedded weights and complete independent legal set | Actor, target, red identity, meld multiset, riichi discard/copy and winning tile checked; genuine PASS differs from unavailable |
| Native engine | Separate Windows native crate test binary | 45 passed |
| Pipe lifetime | Partial stdout watchdog + restart test | Old process is reaped; command/game/settings and watchdog survive reset |
| Window lifetime | Real confirmed move, bridge closes before tracker frame, real tracker gap | No executable stale primary or standby response |
| Python model/session/adapter | Own venv; adapter tests plus actual session hanchans | 62 passed |
| Repository guardrails | Constructed-only categorization and Python formatting tests | 25 passed; formatter check clean |
| Frontend | Source survives tabs, late ready health does not switch source, real gap still blocks | 87 passed; typecheck, Vite build and ESLint passed |
| Windows all targets | Named recovery cache, `--locked --all-targets` | Zero errors/warnings |

The frontend build emits the pre-existing large-chunk advisory. It does not
fail the build. This change adds source text to the existing status row; it
does not add source-selection commands or effects to the frontend.

Reproduce the cross-process tests with the matching Python checkout's own
`.venv` containing its service dependencies, then run the Windows core tests:

```powershell
$env:AKAGI_RECOVERY_WSL_ROOT = '/mnt/d/Games/Mahjong/dev/nanogamekits-recovery'
& '<test-output>/akagi-tests.exe' offline_python --ignored --nocapture --test-threads=1
```

The test peer compiles the actual tiny actor before opening its ephemeral
listener. A file barrier changes only the first actual inference's latency;
the failure variant throws after actual model execution. No test peer chooses
an action. The real local model selects the standby move, which is confirmed
back through the actual tracker/event pipeline before primary catch-up.

The ordinary coordinator fixtures use a controlled primary to isolate source
arbitration and timing while retaining the real NativeBot and rules. They do
not stand in for the separate cross-process evidence. Python rollback tests
compare actual recovered and clean KV arrays exactly and preserve two hand
boundaries. No performance promise for the full production checkpoint/GPU is
inferred from the tiny actor.

Integrated request-binding qualification (based on parent `ba91f78`):

- Raw capture envelopes retain the original producer window; the tracker and
  coordinator never sample a later slot to assign identity. An ahead slot
  cannot supply a missing window or relabel a queued old window.
- Intermediate frame events cannot open a decision. Draw/post-pon frames ending
  in Dora, plus a restored complete prefix, still admit the legal final decision.
- Actual embedded Native direct output and considered PASS carry the same core
  request/control contract. Direct outage is unavailable, with a distinct
  `delivered` count rather than a claimed positioned acknowledgment.
- Fast response admission first applies queued protocol boundaries in order.
- Same-window changes to game/round/event context, including a tracker gap,
  prevent final mouse-down and retry. The original request survives planning.
- The old speculative direct plain-reach follow-up/echo-drop path is removed.
- Tenhou's pre-existing handler/riichi continuation checks are retained; this
  does not claim its full live chain or create a new Tenhou FSM.

The parent still owns final merged release packaging, real-client input
qualification and deployment. Rejoins missing an unknown completed hand remain
explicit gaps; unknown original window budget never creates executable input.

Tenhou REACH confirmation frames still deliver the real event and update the
shared continuation window, but carry no new decision window in the captured
envelope. Tests assert zero additional strategy queries and exactly one
declaring discard in the original plan. Windows all-target check is clean;
combined library regression is 859 passed plus the two separately passed
cross-process recovery tests.
