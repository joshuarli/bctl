#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Statistic {
    Median,
    P95,
    Min,
}

impl Statistic {
    pub fn name(self) -> &'static str {
        match self {
            Statistic::Median => "median",
            Statistic::P95 => "p95",
            Statistic::Min => "min",
        }
    }

    pub fn of(self, sorted: &[f64]) -> f64 {
        match self {
            Statistic::Median => quantile(sorted, 0.5),
            Statistic::P95 => nearest_rank(sorted, 0.95),
            Statistic::Min => sorted[0],
        }
    }
}

pub fn sorted(samples: &[f64]) -> Vec<f64> {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted
}

pub fn quantile(sorted: &[f64], q: f64) -> f64 {
    let position = q * (sorted.len() - 1) as f64;
    let (low, high) = (position.floor() as usize, position.ceil() as usize);
    sorted[low] + (sorted[high] - sorted[low]) * (position - low as f64)
}

pub fn nearest_rank(sorted: &[f64], q: f64) -> f64 {
    let rank = (q * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_interpolates_and_p95_takes_the_nearest_rank() {
        let even = sorted(&[4.0, 1.0, 3.0, 2.0]);
        assert_eq!(Statistic::Median.of(&even), 2.5);
        let hundred: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(Statistic::P95.of(&hundred), 95.0);
        assert_eq!(Statistic::P95.of(&[7.0]), 7.0);
        assert_eq!(Statistic::Min.of(&hundred), 1.0);
    }
}
