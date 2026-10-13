"""Local Python entry point for Rust-owned research contracts."""

from ._native import __version__, smoke_test
from .strategy import AsOfView, Decision, Factor, Strategy, generate_decisions

__all__ = [
    "__version__", "smoke_test", "AsOfView", "Decision", "Factor", "Strategy",
    "generate_decisions",
]
