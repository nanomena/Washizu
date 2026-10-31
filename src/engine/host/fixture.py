"""Offline lifecycle fixture. No model, external connection or game client."""
import json
import os
from pathlib import Path
import socket
import sys
import time
import venv

root = Path.cwd()
phase = (root / "phase").read_text()
if Path(sys.argv[0]).name == "sync":
    (root / "installer.pid").write_text(str(os.getpid()))
    if phase == "installing":
        time.sleep(60)
    venv.EnvBuilder(with_pip=False).create(os.environ["UV_PROJECT_ENVIRONMENT"])
else:
    with (root / "runs").open("a") as handle:
        handle.write(str(os.getpid()) + "\n")
    (root / "told").write_text(os.environ["WASHIZU_ENGINE_DIR"] + chr(10) + os.environ["WASHIZU_ENGINE_ENDPOINT"])
    if phase == "retry":
        raise SystemExit(7)
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        listener.listen()
        (root / "port").write_text(str(listener.getsockname()[1]))
        (root / "service.pid").write_text(str(os.getpid()))
        if phase != "starting":
            print(json.dumps(dict(type="ready")), flush=True)
        if phase == "stubborn":
            time.sleep(60)
        else:
            sys.stdin.buffer.read()
            (root / "eof").write_text("received")
            # Proves that closing stdin alone is not a release acknowledgement.
            time.sleep(0.75)
