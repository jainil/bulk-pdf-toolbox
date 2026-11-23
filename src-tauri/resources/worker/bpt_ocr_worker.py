"""OCR worker for Bulk PDF Toolbox.

Runs exactly one ocrmypdf job and reports structured events to stdout, one JSON
object per line, so the Rust side can show progress without a TTY:

    {"t": "stage", "desc": "OCR", "total": 12, "unit": "page"}
    {"t": "progress", "current": 3, "total": 12}
    {"t": "log", "level": "WARNING", "msg": "..."}
    {"t": "done", "code": 0, "msg": ""}

Usage: python3 bpt_ocr_worker.py '<json job spec>'

On Unix the worker leads its own process group: if the app that launched it
dies, the watchdog thread kills the whole group so no orphaned tesseract
processes keep burning CPU. On Windows the app puts the worker in a
kill-on-close Job Object instead, which the OS tears down with the app.
"""

from __future__ import annotations

import json
import logging
import os
import signal
import sys
import threading
import time
import traceback
from pathlib import Path

_emit_lock = threading.Lock()


def emit(obj: dict) -> None:
    line = json.dumps(obj, ensure_ascii=False)
    with _emit_lock:
        sys.stdout.write(line + "\n")
        sys.stdout.flush()


class EventLogHandler(logging.Handler):
    def emit(self, record: logging.LogRecord) -> None:  # noqa: A003
        try:
            msg = record.getMessage()
        except Exception:  # pragma: no cover - defensive
            msg = str(record.msg)
        # We deliberately pass a huge tesseract timeout (see below); hide the nag.
        if "is very long and may cause issues" in msg:
            return
        emit({"t": "log", "level": record.levelname, "msg": msg})


# The app pauses jobs by freezing the whole process tree (SIGSTOP / NtSuspendProcess). ocrmypdf's tesseract
# timeouts are wall-clock, so after a long pause every in-flight page would
# "time out" and silently lose its text layer. Use a week-long timeout here;
# the app enforces its own stall timeout that excludes paused time.
LONG_TIMEOUT = 7 * 24 * 3600.0


def parent_watchdog(original_ppid: int) -> None:
    """Kill our process group if the launching app goes away (Unix only;
    Windows never re-parents, and the job object covers it there)."""
    while True:
        time.sleep(2)
        if os.getppid() != original_ppid:
            try:
                if hasattr(os, "killpg"):
                    os.killpg(os.getpgrp(), signal.SIGKILL)
            finally:
                os._exit(137)


def main() -> int:
    if len(sys.argv) != 2:
        emit({"t": "done", "code": 1, "msg": "worker expects one JSON argument"})
        return 1
    spec = json.loads(sys.argv[1])

    if os.name != "nt":
        threading.Thread(target=parent_watchdog, args=(os.getppid(),), daemon=True).start()

    handler = EventLogHandler()
    handler.setLevel(logging.INFO)
    for name in ("ocrmypdf",):
        lg = logging.getLogger(name)
        lg.setLevel(logging.INFO)
        lg.addHandler(handler)
        lg.propagate = False
    for noisy in ("pdfminer", "PIL", "pikepdf", "fontTools"):
        logging.getLogger(noisy).setLevel(logging.ERROR)

    try:
        import ocrmypdf
        from ocrmypdf.exceptions import ExitCode, ExitCodeException
    except Exception as e:  # missing / broken runtime
        emit({"t": "done", "code": 3, "msg": f"Failed to load ocrmypdf: {e}"})
        return 3

    kwargs = spec.get("options", {})
    kwargs.setdefault("tesseract_timeout", LONG_TIMEOUT)
    kwargs.setdefault("tesseract_non_ocr_timeout", LONG_TIMEOUT)
    plugin = str(Path(__file__).with_name("bpt_progress_plugin.py"))

    try:
        code = ocrmypdf.ocr(
            spec["input"],
            spec["output"],
            plugins=[plugin],
            progress_bar=True,
            **kwargs,
        )
        code = int(code)
        emit({"t": "done", "code": code, "msg": "" if code == 0 else ExitCode(code).name})
        return code
    except ExitCodeException as e:
        code = int(e.exit_code)
        emit({"t": "done", "code": code, "msg": str(e) or type(e).__name__})
        return code
    except KeyboardInterrupt:
        emit({"t": "done", "code": 130, "msg": "interrupted"})
        return 130
    except Exception as e:  # noqa: BLE001
        emit({"t": "log", "level": "ERROR", "msg": traceback.format_exc()})
        emit({"t": "done", "code": 15, "msg": f"{type(e).__name__}: {e}"})
        return 15


if __name__ == "__main__":
    sys.exit(main())
