use miniserde::{Serialize, json};

use crate::stats::{self, Statistic};

pub const MIN_CLEAN_SAMPLES: usize = 5;

#[derive(Serialize)]
pub struct Discarded {
    pub value: Option<f64>,
    pub reason: String,
}

#[derive(Serialize)]
pub struct Measure {
    pub name: String,
    pub unit: String,
    pub statistic: String,
    pub value: Option<f64>,
    pub n: usize,
    pub min: Option<f64>,
    pub q1: Option<f64>,
    pub q3: Option<f64>,
    pub max: Option<f64>,
    pub samples: Vec<f64>,
    pub discarded: Vec<Discarded>,
    pub status: String,
    pub conditions: String,
}

pub struct Samples {
    values: Vec<f64>,
    discarded: Vec<Discarded>,
}

impl Samples {
    pub fn new() -> Samples {
        Samples { values: Vec::new(), discarded: Vec::new() }
    }

    pub fn push(&mut self, value: f64) {
        self.values.push(value);
    }

    pub fn discard(&mut self, value: Option<f64>, reason: impl Into<String>) {
        self.discarded.push(Discarded { value, reason: reason.into() });
    }

    pub fn finish(&self, name: &str, unit: &str, statistic: Statistic, conditions: &str) -> Measure {
        let sorted = stats::sorted(&self.values);
        let status = if self.values.len() >= MIN_CLEAN_SAMPLES { "clean" } else { "unclean" };
        let some = |f: &dyn Fn(&[f64]) -> f64| (!sorted.is_empty()).then(|| f(&sorted));
        Measure {
            name: name.into(),
            unit: unit.into(),
            statistic: statistic.name().into(),
            value: some(&|s| statistic.of(s)),
            n: self.values.len(),
            min: some(&|s| s[0]),
            q1: some(&|s| stats::quantile(s, 0.25)),
            q3: some(&|s| stats::quantile(s, 0.75)),
            max: some(&|s| s[s.len() - 1]),
            samples: self.values.clone(),
            discarded: self.discarded.iter().map(|d| Discarded { value: d.value, reason: d.reason.clone() }).collect(),
            status: status.into(),
            conditions: conditions.into(),
        }
    }
}

pub fn fact(name: &str, unit: &str, value: f64, conditions: &str) -> Measure {
    Measure {
        name: name.into(),
        unit: unit.into(),
        statistic: "value".into(),
        value: Some(value),
        n: 1,
        min: Some(value),
        q1: Some(value),
        q3: Some(value),
        max: Some(value),
        samples: vec![value],
        discarded: Vec::new(),
        status: "info".into(),
        conditions: conditions.into(),
    }
}

#[derive(Serialize)]
pub struct Host {
    pub model: String,
    pub chip: String,
    pub cpus: u32,
    pub memory_gib: u64,
    pub os: String,
    pub load_average_start: String,
}

#[derive(Serialize)]
pub struct ImageIdentity {
    pub name: String,
    pub bytes: u64,
    pub config_digest: String,
}

#[derive(Serialize)]
pub struct Report {
    pub suite: String,
    pub started: String,
    pub host: Host,
    pub commit: String,
    pub dirty: bool,
    pub runtime: String,
    pub images: Vec<ImageIdentity>,
    pub browser_versions: Vec<String>,
    pub measures: Vec<Measure>,
    pub notes: Vec<String>,
}

impl Report {
    pub fn to_json(&self) -> String {
        json::to_string(self)
    }
}

impl Samples {
    pub fn record(&mut self, value: f64, verdict: crate::host::Verdict) {
        use crate::host::Verdict;
        match verdict {
            Verdict::Clean => self.push(value),
            Verdict::OverlappedLoad => self.discard(Some(value), "host load overlapped the sample"),
        }
    }
}
