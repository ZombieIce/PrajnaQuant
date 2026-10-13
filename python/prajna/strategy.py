"""Single-run, close-session Decision generation; no execution or account API."""

from dataclasses import asdict, dataclass, field
import json
from collections.abc import Callable, Mapping, Sequence
from typing import Any

from ._native import AsOfView, Decision, _generate_decisions


@dataclass(frozen=True)
class FactorDependency:
    """A named dependency on a registered Rust FactorKind and its parameters."""

    name: str
    kind: str
    params: Mapping[str, Any]


@dataclass(frozen=True)
class Strategy:
    strategy_id: str
    version: str
    bar_columns: Sequence[str] = ("close",)
    factors: Sequence[FactorDependency] = field(default_factory=tuple)
    lookback_sessions: int = 1
    rebalance_every: int = 1
    capability: str = "Vectorizable"

    @property
    def identity(self) -> str:
        return f"{self.strategy_id}@{self.version}"


def generate_decisions(
    *,
    lake_root: str,
    dsv: str,
    venue: str,
    members: Sequence[str],
    strategy: Strategy,
    decide: Callable[[AsOfView, dict[str, Any], Decision | None], Decision],
    params: Mapping[str, Any],
    availability_assumption: str = "none",
) -> list[Decision]:
    """Call a pure strategy once per declared Session under the GIL.

    params is JSON round-tripped afresh on every call. prev_decision is the
    strategy's previous validated output, never actual holdings. This is not
    a Python sandbox: deterministic, side-effect-free strategies are required.
    """
    return _generate_decisions(
        lake_root, dsv, venue, list(members),
        json.dumps(asdict(strategy), allow_nan=False),
        decide, json.dumps(dict(params), allow_nan=False),
        availability_assumption,
    )
