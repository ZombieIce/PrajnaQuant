use arrow_array::{Array, Float64Array};
use polars::prelude::*;
use sha2::{Digest, Sha256};
use std::{env, hint::black_box, time::Instant};

struct Input {
    values: Vec<f64>,
    valid: Vec<bool>,
    symbols: usize,
    days: usize,
}

#[derive(Clone, Copy)]
struct Checksum {
    count: usize,
    sum: f64,
    sum_squares: f64,
}

impl Checksum {
    fn new() -> Self {
        Self {
            count: 0,
            sum: 0.0,
            sum_squares: 0.0,
        }
    }

    fn push(&mut self, value: f64) {
        self.count += 1;
        self.sum += value;
        self.sum_squares += value * value;
    }

    fn matches(self, other: Self) -> bool {
        self.count == other.count
            && (self.sum - other.sum).abs() < 1e-8
            && (self.sum_squares - other.sum_squares).abs() < 1e-8
    }
}

fn generate(symbols: usize, days: usize) -> Input {
    let mut values = Vec::with_capacity(symbols * days);
    let mut valid = Vec::with_capacity(symbols * days);
    for symbol in 0..symbols {
        for day in 0..days {
            values.push(
                100.0
                    + symbol as f64 * 0.1
                    + day as f64 * 0.01
                    + ((symbol * 7 + day * 13) % 17) as f64 * 0.001,
            );
            valid.push(!(symbol * days + day).is_multiple_of(97));
        }
    }
    Input {
        values,
        valid,
        symbols,
        days,
    }
}

fn input_sha256(input: &Input) -> String {
    let mut digest = Sha256::new();
    for (value, valid) in input.values.iter().zip(&input.valid) {
        digest.update(value.to_le_bytes());
        digest.update([u8::from(*valid)]);
    }
    format!("{:x}", digest.finalize())
}

fn validate_hand_case() -> Result<(), Box<dyn std::error::Error>> {
    // Two five-day series, lookback two. The only valid returns are written
    // out independently from the loop under measurement.
    let input = Input {
        values: vec![
            100.0, 102.0, 0.0, 110.0, 121.0, 100.0, 100.0, 110.0, 121.0, 133.1,
        ],
        valid: vec![true, true, false, true, true, true, true, true, true, true],
        symbols: 2,
        days: 5,
    };
    let mut expected = Checksum::new();
    for value in [0.078_431_372_549_019_6, 0.1, 0.21, 0.21] {
        expected.push(value);
    }
    assert!(soa(&input, 2).matches(expected));
    assert!(arrow(&arrow_columns(&input), 2).matches(expected));
    assert!(polars(&polars_columns(&input), 2)?.matches(expected));
    Ok(())
}

fn soa(input: &Input, lookback: usize) -> Checksum {
    let mut out = Checksum::new();
    for symbol in 0..input.symbols {
        let base = symbol * input.days;
        for day in lookback..input.days {
            let now = base + day;
            let before = now - lookback;
            if input.valid[now] && input.valid[before] {
                out.push(input.values[now] / input.values[before] - 1.0);
            }
        }
    }
    out
}

fn arrow_columns(input: &Input) -> Vec<Float64Array> {
    (0..input.symbols)
        .map(|symbol| {
            let base = symbol * input.days;
            (0..input.days)
                .map(|day| {
                    let at = base + day;
                    input.valid[at].then_some(input.values[at])
                })
                .collect::<Float64Array>()
        })
        .collect()
}

fn arrow(columns: &[Float64Array], lookback: usize) -> Checksum {
    let mut out = Checksum::new();
    for column in columns {
        for day in lookback..column.len() {
            let before = day - lookback;
            if column.is_valid(day) && column.is_valid(before) {
                out.push(column.value(day) / column.value(before) - 1.0);
            }
        }
    }
    out
}

fn polars_columns(input: &Input) -> Vec<Series> {
    (0..input.symbols)
        .map(|symbol| {
            let base = symbol * input.days;
            let values = (0..input.days)
                .map(|day| {
                    let at = base + day;
                    input.valid[at].then_some(input.values[at])
                })
                .collect::<Vec<_>>();
            Series::new("close".into(), values)
        })
        .collect()
}

fn polars(columns: &[Series], lookback: usize) -> Result<Checksum, PolarsError> {
    let mut out = Checksum::new();
    for column in columns {
        let close = column.f64()?;
        for day in lookback..close.len() {
            if let (Some(now), Some(before)) = (close.get(day), close.get(day - lookback)) {
                out.push(now / before - 1.0);
            }
        }
    }
    Ok(out)
}

fn measure<F: FnMut() -> Checksum>(
    name: &str,
    mut calculate: F,
    expected: Checksum,
    repetitions: usize,
) {
    let _ = black_box(calculate());
    let mut elapsed = Vec::with_capacity(repetitions);
    for _ in 0..repetitions {
        let start = Instant::now();
        let actual = black_box(calculate());
        assert!(actual.matches(expected), "{name} differs from reference");
        elapsed.push(start.elapsed().as_micros());
    }
    elapsed.sort_unstable();
    let median = elapsed[elapsed.len() / 2];
    let p95 = elapsed[(elapsed.len() * 95 / 100).min(elapsed.len() - 1)];
    println!(
        "{{\"kind\":\"candidate\",\"candidate\":\"{name}\",\"median_us\":{median},\"p95_us\":{p95},\"samples_us\":{elapsed:?},\"factor_values\":{},\"sum\":{:.12},\"sum_squares\":{:.12}}}",
        expected.count, expected.sum, expected.sum_squares
    );
}

fn parse_arg(args: &[String], at: usize, default: usize) -> usize {
    args.get(at).map_or(default, |value| {
        value.parse().expect("expected positive integer")
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    validate_hand_case()?;
    let args = env::args().collect::<Vec<_>>();
    let symbols = parse_arg(&args, 1, 128);
    let days = parse_arg(&args, 2, 4096);
    let lookback = parse_arg(&args, 3, 20);
    let repetitions = parse_arg(&args, 4, 10);
    assert!(symbols > 0 && days > lookback && lookback > 0 && repetitions > 0);

    let input = generate(symbols, days);
    println!(
        "{{\"kind\":\"input\",\"symbols\":{symbols},\"days\":{days},\"lookback\":{lookback},\"repetitions\":{repetitions},\"input_sha256\":\"{}\"}}",
        input_sha256(&input)
    );
    let expected = soa(&input, lookback);
    let arrow_data = arrow_columns(&input);
    let polars_data = polars_columns(&input);
    assert!(arrow(&arrow_data, lookback).matches(expected));
    assert!(polars(&polars_data, lookback)?.matches(expected));

    eprintln!("symbols={symbols} days={days} lookback={lookback} repetitions={repetitions}");
    measure("soa", || soa(&input, lookback), expected, repetitions);
    measure(
        "arrow",
        || arrow(&arrow_data, lookback),
        expected,
        repetitions,
    );
    measure(
        "polars",
        || polars(&polars_data, lookback).expect("polars calculation"),
        expected,
        repetitions,
    );
    Ok(())
}
