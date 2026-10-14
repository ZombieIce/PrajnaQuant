"""Execution evidence for trusted deterministic Python decision functions."""

import hashlib
import inspect
import marshal
from pathlib import Path
import subprocess
import sys


def capture(reference, declaration, decide):
    if not inspect.isfunction(decide):
        raise TypeError("decide must be a Python function")
    code = marshal.dumps(decide.__code__)
    source_file = inspect.getsourcefile(decide)
    path = Path(source_file) if source_file and not source_file.startswith("<") else None
    if path is None or not path.is_file():
        source_file = None
        source = code
    else:
        source_file = str(path.resolve())
        source = path.read_bytes()
    frozen = subprocess.check_output(
        [sys.executable, "-m", "pip", "freeze", "--all"], text=True,
    )
    return {
        "strategy_reference": reference,
        "declaration_json": declaration,
        "source_sha256": hashlib.sha256(source).hexdigest(),
        "source_file": source_file,
        "callable_sha256": hashlib.sha256(code).hexdigest(),
        "callable_name": f"{decide.__module__}.{decide.__qualname__}",
        "python_version": sys.version,
        "dependency_lock": "\n".join(sorted(frozen.splitlines())) + "\n",
        "dependency_lock_source": "sys.executable -m pip freeze --all",
    }
