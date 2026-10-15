//! The Python host prepares Decisions before entering the existing execution schedule.
use super::*;
use prajna_research::strategy::Decision;

pub type PreparedDecisions = BTreeMap<String, Result<Vec<Decision>, String>>;

/// Host provenance is Execution identity, never Experiment or Run Spec identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PythonProvenance {
    pub strategy_reference: String,
    pub declaration_json: String,
    pub source_sha256: String,
    pub source_file: Option<String>,
    pub callable_sha256: String,
    pub callable_name: String,
    pub python_version: String,
    pub dependency_lock: String,
    pub dependency_lock_source: String,
}

impl Experiment {
    /// JSON values for callbacks, kept separate from identity-bearing tagged float/string values.
    pub fn python_parameters(
        &self,
        lake: impl AsRef<Path>,
    ) -> Result<BTreeMap<String, Value>, ExperimentError> {
        if !engine::is_python_strategy(&self.strategy_kind) {
            return Err(invalid(
                "Python parameters require a Python strategy reference",
            ));
        }
        let ParameterSpace::Native { combinations, .. } = &self.parameter_space else {
            return Err(invalid("missing Python parameter space"));
        };
        if self.pit.is_some() {
            return Ok(self
                .run_definitions(lake)?
                .into_iter()
                .map(|run| (run.run_id, run.run_spec["strategy"]["parameters"].clone()))
                .collect());
        }
        let mut parameters = BTreeMap::new();
        for combination in combinations {
            let definitions = engine::python_run_specs(self, std::slice::from_ref(combination))?;
            parameters.insert(definitions[0].run_id.clone(), combination.clone());
        }
        Ok(parameters)
    }

    pub(super) fn prepared_runs(
        &self,
        lake: &Path,
        prepared: &PreparedDecisions,
    ) -> Result<Vec<ExpandedRun>, ExperimentError> {
        if !engine::is_python_strategy(&self.strategy_kind) {
            return Err(invalid(
                "prepared Decisions require a Python strategy reference",
            ));
        }
        let definitions = self.run_definitions(lake)?;
        let expected = definitions
            .iter()
            .map(|run| &run.run_id)
            .collect::<BTreeSet<_>>();
        if expected != prepared.keys().collect::<BTreeSet<_>>() {
            return Err(invalid(
                "prepared Decisions must match every expanded Run exactly",
            ));
        }
        Ok(definitions
            .into_iter()
            .map(|run| ExpandedRun {
                strategy: RunStrategy::Prepared(prepared[&run.run_id].clone()),
                run_id: run.run_id,
                run_spec: run.run_spec,
                parameters: None,
                factor: None,
            })
            .collect())
    }

    /// All Python callbacks have finished. This method contains only Rust work;
    /// the binding must detach the GIL for the entire call, including storage/replay.
    #[allow(clippy::too_many_arguments)]
    pub fn execute_python_and_store(
        &self,
        lake: impl AsRef<Path>,
        repository: impl AsRef<Path>,
        level: ResultLevel,
        threads: usize,
        prepared: &PreparedDecisions,
        provenance: PythonProvenance,
    ) -> Result<StoredExecution, ExperimentError> {
        if provenance.strategy_reference != self.strategy_kind {
            return Err(invalid("Python provenance strategy reference mismatch"));
        }
        let lake = lake.as_ref();
        let experiment_id = self.id()?;
        let mut identity = ExecutionIdentity::capture(&experiment_id, repository)?;
        let base_id = identity.id.clone();
        identity.reproducible &= provenance.source_file.is_some();
        identity.python = Some(provenance);
        identity.id = super::identity(
            "exe",
            &restricted_jcs(&serde_json::json!({
                "base_execution": base_id, "python": identity.python,
            }))
            .map_err(|error| invalid(error.to_string()))?,
        );
        let effective = storage::replay_level(lake, &experiment_id, &identity.id, level)?;
        let started = storage::now_rfc3339();
        let report = self.execute_prepared(lake, threads, effective, Some(prepared))?;
        storage::store_execution(self, &identity, &report, lake, level, threads, started)
    }
}
