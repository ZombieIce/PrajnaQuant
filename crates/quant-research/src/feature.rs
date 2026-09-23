pub fn pct_change(values: &[f64], periods: usize) -> Vec<Option<f64>> {
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            (index >= periods && values[index - periods] != 0.0)
                .then(|| *value / values[index - periods] - 1.0)
        })
        .collect()
}

pub fn rolling_mean(values: &[f64], window: usize) -> Vec<Option<f64>> {
    (0..values.len())
        .map(|index| {
            (window > 0 && index + 1 >= window)
                .then(|| values[index + 1 - window..=index].iter().sum::<f64>() / window as f64)
        })
        .collect()
}

pub fn rolling_std(values: &[f64], window: usize) -> Vec<Option<f64>> {
    if window < 2 {
        return vec![None; values.len()];
    }
    (0..values.len())
        .map(|index| {
            (index + 1 >= window).then(|| {
                let s = &values[index + 1 - window..=index];
                let m = s.iter().sum::<f64>() / window as f64;
                (s.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (window - 1) as f64).sqrt()
            })
        })
        .collect()
}

pub fn rolling_drawdown(values: &[f64], window: usize) -> Vec<Option<f64>> {
    (0..values.len())
        .map(|index| {
            (index + 1 >= window).then(|| {
                let s = &values[index + 1 - window..=index];
                values[index] / s.iter().copied().fold(f64::NEG_INFINITY, f64::max) - 1.0
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lookback_values_are_missing() {
        assert_eq!(pct_change(&[1., 2., 4.], 2), vec![None, None, Some(3.)]);
        assert_eq!(rolling_std(&[1., 2., 3.], 1), vec![None, None, None]);
    }
}
