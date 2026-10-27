# Offline recovery qualification
Last verified: 2026-10-26

This is a development-worktree qualification. No application/account was
opened, no live table was operated, and no installed configuration, runtime
checkout or production weight was changed. The matching Python change lives
in the `nanogamekits-recovery` worktree.
Matching Python commit: `62f3370d84752b3ee2cf8c1d14abc42e87201aa7`.

| Boundary | Evidence | Result |
|---|---|---|
| Core regression | Windows library test executable, four threads | 858 passed; two environment-dependent tests run separately |
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
| Frontend | Source survives tabs, late ready health does not switch source, real gap still blocks | 78 passed; typecheck, Vite build and ESLint passed |
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

Integration remains required: the UI FSM must carry the original envelope
unchanged to final input permission and bind its bridge window to the original
request as described in [CONTROL.md](CONTROL.md). Automatic-off, stage changes,
mouse-down, real client rendering and Tenhou's complete live chain are owned
by that combined regression, not certified here. Rejoins missing an unknown
completed hand or contradicting the canonical prefix remain explicit gaps.
Unknown real window budget produces no executable answer.
