# Service ownership through account shutdown

`service::start` returns one `ServiceHandle`. Headless keeps it until EOF on
its own stdin, then calls and awaits `shutdown()`. One watch signal cancels
idle, dependency installation, model loading, ready and retry states. Closing
the handle also signals cancellation. The service's retry loop cannot start
another process once it observes cancellation.

Both the service and its uv installer own a `Child`. Cancelling a service
closes its stdin and waits for process exit for three seconds. On expiry it
logs the PID and termination, requests a kill, then waits up to two more
seconds to reap it. The installer uses the same bounded process ownership.
Retargeting also reaps the old service before starting another. A failure to
confirm release is fatal to that service task, rather than a retry that
could overlap the old process. Normal uv failures retain the existing retry.

Headless first stops capture (existing two-second shutdown budget), awaits
the service, then asks its sole stdout writer to flush a terminal
`shutdown-complete` event with `{ok, error}`. Cleanup failure exits nonzero.
The shell requires both `ok: true` and core exit zero. It then sends
`profile-released` over the account process's IPC channel. The chooser only
releases the slot after that acknowledgement **and** the account process
actually closes. A failed spawn with no OS process has no service ownership;
a crash or timeout of an existing process leaves its account slot locked.

There is no runtime port polling, process-name kill or arbitrary desktop
delay. Logs include the owned PID and `owned process released and reaped`.
This protocol covers controlled teardown. An abrupt core/account crash does
not claim successful cleanup; automatic repair of that locked state would
need separately tracked process ownership, such as a Windows Job Object.

## Offline verification

The opt-in Rust process fixtures use a real Python interpreter with a fresh
temporary, dependency-free venv. No model, production settings or game page
is loaded. Set `AKAGI_TEST_PYTHON` to an absolute Python executable, and set
`PYTHONDONTWRITEBYTECODE=1` when using a shared interpreter. Then run:

```sh
cargo test --lib bot::service::lifecycle_tests -- --ignored --nocapture --test-threads=1
```

For a cross-built Windows test executable, run the same test filter with
`--ignored --nocapture --test-threads=1` in PowerShell after setting those
environment variables. Each fixture reports its actual PID, temporary port
and teardown duration. Covered phases:

- Ready and loading: keep a listener alive for 750 ms after stdin EOF;
  shutdown must remain pending, and the port must bind again immediately
  after shutdown returns.
- Ignored EOF: terminate and reap the service after the grace budget.
- Installing: terminate and reap the installer, with no model process or
  successful installation stamp created.
- Retry: stop after the failed first process and verify that the retry
  deadline cannot produce a replacement.
- Cancellation before start: no install or service process is started.

`tests/bot_service.rs` also exercises a real interpreter and uv, including
failure, restart, resolved settings and shutdown with a delayed port release.
Run `cargo test --test bot_service -- --nocapture` where python3 and uv are
available. Its runtime-discovery skip is printed when unavailable.

The complete headless EOF/acknowledgement path can be tested against a built
core, again using a temporary fixture without loading a model:

```sh
node desktop/tests/service-teardown.mjs --core=/absolute/akagi-core --python=/absolute/python --uv=/absolute/uv
```

It runs cooperative and EOF-ignoring services through the actual Rust core,
checks their PID is gone and their temporary port can be rebound, and requires
the flushed cleanup acknowledgement plus core exit zero. No port polling is
used to decide when shutdown is complete.

In `desktop`, `npm run check`, `npm test`, and
`xvfb-run -a npm run smoke:profiles` verify acknowledgement failures, locked
slots after crashes and the real chooser/account-window sequence. The fake
core acknowledges cleanup only after writing its delayed release marker.
