"""Typed definitions; Rust owns validation, canonical identities and execution."""

from collections.abc import Callable
from dataclasses import asdict, dataclass
from enum import StrEnum
import json
from pathlib import Path
from typing import Literal

from . import _native
from .strategy import AsOfView, Decision, Strategy


type JsonValue = None | bool | int | float | str | list[JsonValue] | dict[str, JsonValue]
type Parameters = dict[str, JsonValue]
type PathLike = str | Path


class AvailabilityAssumption(StrEnum):
    NONE = "none"
    TREAT_UNKNOWN_AS_TS_CLOSE = "treat_unknown_as_ts_close"


class EngineKind(StrEnum):
    VECTOR = "vector"
    FAST_EVENT = "fast_event"
    ACCURATE_EVENT = "accurate_event"


class RustStrategy(StrEnum):
    S2_ROTATION = "s2_rotation"
    S2_PIT_ROTATION = "s2_pit_rotation"
    BUY_AND_HOLD = "buy_and_hold"
    MA_CROSSOVER = "ma_crossover"


class ResultLevel(StrEnum):
    SUMMARY = "summary"
    STANDARD = "standard"
    FULL = "full"


@dataclass(frozen=True)
class DatasetVersion:
    id: str


@dataclass(frozen=True)
class Universe:
    id: str


@dataclass(frozen=True)
class Venue:
    id: str


@dataclass(frozen=True)
class PythonStrategy:
    strategy_id: str
    version: str

    def reference(self) -> str:
        for part in (self.strategy_id, self.version):
            if not part or "@" in part or any(char.isspace() for char in part):
                raise ValueError("Python strategy requires nonempty strategy_id and version without whitespace or @")
        return f"{self.strategy_id}@{self.version}"


@dataclass(frozen=True)
class Costs:
    commission_rate: float = 0.0
    buy_slippage_bps: float = 0.0
    sell_slippage_bps: float = 0.0
    buy_tax_rate: float = 0.0
    sell_tax_rate: float = 0.0

    def to_definition(self) -> Parameters:
        return {
            "commission_rate": self.commission_rate,
            "buy_slippage_bps": self.buy_slippage_bps,
            "sell_slippage_bps": self.sell_slippage_bps,
            "buy_tax_rate": self.buy_tax_rate,
            "sell_tax_rate": self.sell_tax_rate,
        }


@dataclass(frozen=True)
class Backend:
    name: str
    version: str


@dataclass(frozen=True)
class Capital:
    id: int
    capital: str


@dataclass(frozen=True)
class LotSizeSource:
    values: dict[str, str] | None = None

    def to_definition(self) -> Parameters:
        if self.values is None:
            return {"kind": "instrument"}
        return {"kind": "explicit", "values": dict(self.values)}


@dataclass(frozen=True, kw_only=True)
class FastEventSettings:
    currency: str
    initial_cash: str
    sizing: Literal["vector_parity", "lot"]
    rebalance: Literal["full_target", "entry_only"]
    minimum_commission: str
    portfolios: list[Capital]
    lot_size_source: LotSizeSource
    entry_budget: Literal["fixed_initial", "remaining_cash"] | None = None

    def to_definition(self) -> Parameters:
        result: Parameters = {
            "currency": self.currency,
            "initial_cash": self.initial_cash,
            "sizing": self.sizing,
            "rebalance": self.rebalance,
            "minimum_commission": self.minimum_commission,
            "portfolios": [
                {"id": capital.id, "capital": capital.capital}
                for capital in self.portfolios
            ],
            "lot_size_source": self.lot_size_source.to_definition(),
        }
        if self.entry_budget is not None:
            result["entry_budget"] = self.entry_budget
        return result


@dataclass(frozen=True)
class Engine:
    kind: EngineKind
    settings: FastEventSettings | None = None
    backend: Backend | None = None
    initial_capital: str | None = None


@dataclass(frozen=True)
class Grid:
    axes: dict[str, list[JsonValue]]

    def to_definition(self) -> Parameters:
        return {"grid": {name: list(values) for name, values in self.axes.items()}}


@dataclass(frozen=True)
class List:
    combinations: list[Parameters]

    def to_definition(self) -> Parameters:
        return {"list": [dict(parameters) for parameters in self.combinations]}


@dataclass(frozen=True)
class RunDefinition:
    run_id: str
    run_spec: Parameters


@dataclass(frozen=True)
class ExperimentIdentity:
    experiment_id: str
    canonical_json: str
    runs: tuple[RunDefinition, ...]


@dataclass(frozen=True)
class ExecutionResult:
    manifest: Parameters
    path: Path
    statistics: Parameters
    action: str


def _validate_json(value: JsonValue) -> None:
    if isinstance(value, dict):
        for key, item in value.items():
            if not isinstance(key, str):
                raise TypeError("JSON object keys must be strings")
            _validate_json(item)
    elif isinstance(value, list):
        for item in value:
            _validate_json(item)
    elif value is not None and not isinstance(value, (bool, int, float, str)):
        raise TypeError(f"not a JSON value: {type(value).__name__}")


@dataclass(frozen=True, kw_only=True)
class Experiment:
    dataset_version: DatasetVersion
    universe: Universe
    venue: Venue
    strategy: RustStrategy | PythonStrategy
    engine: Engine
    costs: Costs
    parameter_space: Grid | List
    availability_assumption: AvailabilityAssumption = AvailabilityAssumption.NONE
    sessions_per_year: int = 252
    unfilled_entry: Literal["skip", "retry"] | None = None
    pit: Parameters | None = None

    def to_definition(self) -> Parameters:
        result: Parameters = {
            "experiment_version": 1,
            "dsv": self.dataset_version.id,
            "universe": self.universe.id,
            "venue": self.venue.id,
            "strategy": (
                self.strategy.reference()
                if isinstance(self.strategy, PythonStrategy) else self.strategy.value
            ),
            "engine": self.engine.kind.value,
            "availability_assumption": self.availability_assumption.value,
            "costs": self.costs.to_definition(),
            "sessions_per_year": self.sessions_per_year,
            "parameter_space": self.parameter_space.to_definition(),
        }
        if self.pit is not None:
            result["pit"] = self.pit
        if self.engine.initial_capital is not None:
            result["vector_pit"] = {"initial_capital": self.engine.initial_capital}
        if self.unfilled_entry is not None:
            result["unfilled_entry"] = self.unfilled_entry
        if self.engine.settings is not None:
            result["fast_event"] = self.engine.settings.to_definition()
        if self.engine.backend is not None:
            result["backend"] = {
                "name": self.engine.backend.name,
                "version": self.engine.backend.version,
            }
        return result

    def to_json(self) -> str:
        definition = self.to_definition()
        _validate_json(definition)
        return json.dumps(definition, allow_nan=False)

    def describe(self, lake: PathLike) -> ExperimentIdentity:
        result = json.loads(_native._describe_experiment(self.to_json(), Path(lake)))
        return ExperimentIdentity(
            result["experiment_id"],
            result["canonical_json"],
            tuple(RunDefinition(run["run_id"], run["run_spec"]) for run in result["runs"]),
        )

    def run(
        self,
        lake: PathLike,
        *,
        repository: PathLike | None = None,
        level: ResultLevel = ResultLevel.SUMMARY,
        threads: int | None = None,
        backend_python: PathLike | None = None,
        backend_worker: PathLike | None = None,
        strategy: Strategy | None = None,
        decide: Callable[[AsOfView, Parameters, Decision | None], Decision] | None = None,
    ) -> ExecutionResult:
        if strategy is not None or decide is not None:
            from .provenance import capture
            if not isinstance(self.strategy, PythonStrategy):
                raise ValueError("callback execution requires a PythonStrategy reference")
            if not isinstance(strategy, Strategy) or not callable(decide):
                raise TypeError("Python execution requires Strategy and a callable decide")
            if strategy.identity != self.strategy.reference():
                raise ValueError("Strategy declaration must match Experiment strategy reference")
            declaration = json.dumps(asdict(strategy), sort_keys=True, allow_nan=False)
            result = json.loads(_native._run_python_experiment(
                self.to_json(), Path(lake),
                Path.cwd() if repository is None else Path(repository),
                level.value, declaration, decide,
                json.dumps(capture(strategy.identity, declaration, decide)),
                threads,
                None if backend_python is None else Path(backend_python),
                None if backend_worker is None else Path(backend_worker),
            ))
            return ExecutionResult(
                result["manifest"], Path(result["path"]), result["statistics"], result["action"],
            )
        result = json.loads(_native._run_experiment(
            self.to_json(),
            Path(lake),
            Path.cwd() if repository is None else Path(repository),
            level.value,
            threads,
            None if backend_python is None else Path(backend_python),
            None if backend_worker is None else Path(backend_worker),
        ))
        return ExecutionResult(
            result["manifest"], Path(result["path"]), result["statistics"], result["action"],
        )
