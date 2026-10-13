"""Local Python entry point for Rust-owned research contracts."""

from ._native import (
    EngineNotProducedError, Factor, ResultNotSavedError, __version__,
    read_dataset, read_factor, read_run, smoke_test,
)
from .experiment import (
    AvailabilityAssumption,
    Backend,
    Capital,
    Costs,
    DatasetVersion,
    Engine,
    EngineKind,
    ExecutionResult,
    Experiment,
    ExperimentIdentity,
    FastEventSettings,
    Grid,
    List,
    LotSizeSource,
    PythonStrategy,
    ResultLevel,
    RunDefinition,
    RustStrategy,
    Universe,
    Venue,
)
from .strategy import AsOfView, Decision, FactorDependency, Strategy, generate_decisions

__all__ = [
    "__version__", "smoke_test", "AvailabilityAssumption", "Backend", "Capital",
    "Costs", "DatasetVersion", "Engine", "EngineKind", "ExecutionResult",
    "Experiment", "ExperimentIdentity", "FastEventSettings", "Grid", "List",
    "LotSizeSource", "PythonStrategy", "ResultLevel", "RunDefinition", "RustStrategy",
    "Universe", "Venue",
    "Factor", "read_dataset", "read_factor", "read_run",
    "ResultNotSavedError", "EngineNotProducedError",
    "AsOfView", "Decision", "FactorDependency", "Strategy", "generate_decisions",
]
