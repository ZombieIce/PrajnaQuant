use std::{fs, path::Path};

use prajna_experiment::summarize;
use prajna_research::vector::VectorResult;
use serde_json::Value;

const EXPECTED_FILES: [&str; 3] = [
    "dataset-v1.json",
    "b2-s2-scale-64x252-v2.json",
    "b2-s2-scale-64x252-v2.trend20.json",
];
#[test]
fn rust_summary_matches_independent_python_golden_for_all_s2_cases() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for filename in EXPECTED_FILES {
        let path = repo_root.join("poc/mvp1-golden/expected").join(filename);
        let bytes = fs::read(&path)
            .unwrap_or_else(|error| panic!("could not read {}: {error}", path.display()));
        let expected: Value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|error| panic!("invalid {} JSON: {error}", path.display()));
        let summary_tolerance = expected
            .pointer("/tolerance/nav_abs")
            .and_then(Value::as_f64)
            .unwrap_or_else(|| panic!("{} has no Summary tolerance", path.display()));
        let vector: VectorResult = serde_json::from_value(
            expected
                .get("vector")
                .unwrap_or_else(|| panic!("{} has no vector", path.display()))
                .clone(),
        )
        .unwrap_or_else(|error| panic!("invalid {} Vector: {error}", path.display()));
        let actual = serde_json::to_value(summarize(&vector, 252))
            .unwrap_or_else(|error| panic!("could not serialize {} Summary: {error}", filename));
        assert_json_close(
            &actual,
            expected
                .get("summary")
                .unwrap_or_else(|| panic!("{} has no Python Summary", path.display())),
            filename,
            "summary",
            summary_tolerance,
        );
    }
}

fn assert_json_close(actual: &Value, expected: &Value, filename: &str, path: &str, tolerance: f64) {
    match (actual, expected) {
        (Value::Object(actual), Value::Object(expected)) => {
            assert_eq!(
                actual.keys().collect::<Vec<_>>(),
                expected.keys().collect::<Vec<_>>(),
                "{filename}: keys differ at {path}"
            );
            for (key, expected_value) in expected {
                assert_json_close(
                    actual.get(key).expect("object keys were checked"),
                    expected_value,
                    filename,
                    &format!("{path}.{key}"),
                    tolerance,
                );
            }
        }
        (Value::Array(actual), Value::Array(expected)) => {
            assert_eq!(actual.len(), expected.len(), "{filename}: {path} length");
            for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
                assert_json_close(
                    actual,
                    expected,
                    filename,
                    &format!("{path}[{index}]"),
                    tolerance,
                );
            }
        }
        (Value::Number(actual), Value::Number(expected)) => {
            let actual = actual
                .as_f64()
                .expect("summary number is representable as f64");
            let expected = expected
                .as_f64()
                .expect("golden number is representable as f64");
            assert!(
                (actual - expected).abs() <= tolerance,
                "{filename}: {path}: actual {actual}, expected {expected}"
            );
        }
        _ => assert_eq!(actual, expected, "{filename}: {path}"),
    }
}
