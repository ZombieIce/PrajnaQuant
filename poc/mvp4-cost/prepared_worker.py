"""Benchmark-only persistent process; production still starts one process per Run."""

from pathlib import Path
import resource
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "backends/nautilus"))
import worker


def usage(kind):
    raw = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    worker.send({
        "type": kind, "rss_raw": raw,
        "rss_unit": "bytes" if sys.platform == "darwin" else "KiB",
        "rss_bytes": raw if sys.platform == "darwin" else raw * 1024,
    })


def main():
    usage("baseline")
    prepared = None
    packet = None
    while True:
        request = worker.receive()
        if request["type"] == "shutdown":
            return
        if request.get("protocol") != worker.VERSION:
            raise ValueError("unsupported protocol")
        path = Path(request["packet"])
        if request["type"] == "prepare":
            start = time.perf_counter_ns()
            prepared = worker.prepare(path)
            packet = path
            worker.send({"type": "prepared", "conversion_ns": time.perf_counter_ns() - start})
        elif request["type"] == "begin_run":
            if prepared is None or path != packet:
                raise ValueError("Run requires matching prepared packet")
            diagnostic = path / "backend-diagnostics.jsonl"
            if diagnostic.exists():
                diagnostic.unlink()
            worker.run(request, prepared)
            usage("usage")
        else:
            raise ValueError("unsupported benchmark request")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, KeyError, TypeError, EOFError, OSError, RuntimeError) as error:
        worker.send({"type": "error", "reason": f"{type(error).__name__}: {error}"})
        print(str(error), file=sys.stderr)
        sys.exit(1)
