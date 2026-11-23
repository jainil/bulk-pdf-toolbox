"""ocrmypdf plugin that reports progress as JSON lines on stdout."""

from __future__ import annotations

import json
import sys
import threading
import time

from ocrmypdf import hookimpl

_lock = threading.Lock()


def _emit(obj: dict) -> None:
    with _lock:
        sys.stdout.write(json.dumps(obj) + "\n")
        sys.stdout.flush()


class JsonProgressBar:
    def __init__(self, *, total=None, desc=None, unit=None, disable=False, **kwargs):
        self.total = total
        self.desc = desc
        self.unit = unit
        self.disable = disable
        self.current = 0.0
        self._last = 0.0

    def __enter__(self):
        if not self.disable:
            _emit({"t": "stage", "desc": self.desc, "total": self.total, "unit": self.unit})
        return self

    def __exit__(self, exc_type, exc_value, traceback):
        if not self.disable and exc_type is None and self.total:
            _emit({"t": "progress", "current": self.total, "total": self.total})
        return False

    def update(self, n=1, *, completed=None):
        if completed is not None:
            self.current = completed
        else:
            self.current += n
        if self.disable:
            return
        now = time.monotonic()
        done = bool(self.total) and self.current >= self.total
        if done or now - self._last >= 0.25:
            self._last = now
            _emit({"t": "progress", "current": self.current, "total": self.total})


@hookimpl
def get_progressbar_class():
    return JsonProgressBar
