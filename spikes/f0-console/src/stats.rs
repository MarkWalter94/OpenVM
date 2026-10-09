//! Percentili per le misure della sezione 13 (metodo nearest-rank).

/// Riepilogo di un campione.
#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    /// Numero di campioni.
    pub n: usize,
    /// Minimo.
    pub min: f64,
    /// Mediana (p50).
    pub p50: f64,
    /// 95° percentile.
    pub p95: f64,
    /// Massimo.
    pub max: f64,
    /// Media.
    pub mean: f64,
}

/// Percentile nearest-rank su campione ordinato.
pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil().max(1.0) as usize;
    sorted[rank.min(sorted.len()) - 1]
}

/// Calcola il riepilogo.
pub fn summarize(samples: &[f64]) -> Summary {
    let mut s = samples.to_vec();
    s.sort_by(f64::total_cmp);
    Summary {
        n: s.len(),
        min: s.first().copied().unwrap_or(f64::NAN),
        p50: percentile(&s, 50.0),
        p95: percentile(&s, 95.0),
        max: s.last().copied().unwrap_or(f64::NAN),
        mean: s.iter().sum::<f64>() / s.len().max(1) as f64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank() {
        let v: Vec<f64> = (1..=100).map(f64::from).collect();
        let s = summarize(&v);
        assert_eq!((s.p50, s.p95, s.max, s.min), (50.0, 95.0, 100.0, 1.0));
    }
}
