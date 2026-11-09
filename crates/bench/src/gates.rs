use std::collections::BTreeMap;
use std::path::PathBuf;

use miniserde::{Deserialize, json};

use crate::host::{self, Fallible};

#[derive(Deserialize)]
struct GateFile {
    gates: Vec<Gate>,
}

#[derive(Deserialize)]
struct Gate {
    measure: String,
    kind: String,
    max: Option<f64>,
    min: Option<f64>,
}

#[derive(Deserialize)]
struct Result {
    measures: Vec<Measured>,
}

#[derive(Deserialize)]
struct Measured {
    name: String,
    value: Option<f64>,
    status: String,
}

fn latest_per_suite() -> Fallible<Vec<PathBuf>> {
    let dir = host::root().join("tests/bench/results");
    let mut latest: BTreeMap<String, PathBuf> = BTreeMap::new();
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?.flatten().map(|e| e.path()).collect();
    files.sort();
    for file in files {
        let suite = file.file_stem().and_then(|s| s.to_str()).and_then(|s| s.split_once('-')).map(|(_, suite)| suite.to_string());
        if let Some(suite) = suite {
            latest.insert(suite, file);
        }
    }
    Ok(latest.into_values().collect())
}

pub fn check(results: &[String]) -> Fallible<()> {
    let gate_path = host::root().join("tests/bench/gates.json");
    let text = std::fs::read_to_string(&gate_path).map_err(|e| format!("{}: {e}", gate_path.display()))?;
    let gates: GateFile = json::from_str(&text).map_err(|_| format!("{} is not a gate file", gate_path.display()))?;
    let files = if results.is_empty() { latest_per_suite()? } else { results.iter().map(PathBuf::from).collect() };
    let mut measured: BTreeMap<String, Measured> = BTreeMap::new();
    for file in &files {
        let text = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
        let result: Result = json::from_str(&text).map_err(|_| format!("{} is not a result file", file.display()))?;
        for measure in result.measures {
            measured.insert(measure.name.clone(), measure);
        }
    }
    let mut violations = 0;
    for gate in &gates.gates {
        let Some(measure) = measured.get(&gate.measure) else {
            println!("not measured  {}", gate.measure);
            continue;
        };
        let Some(value) = measure.value else {
            println!("no value      {}", gate.measure);
            continue;
        };
        if gate.kind == "time" && measure.status != "clean" {
            println!("deferred      {} (samples are {})", gate.measure, measure.status);
            continue;
        }
        let over = gate.max.is_some_and(|max| value > max);
        let under = gate.min.is_some_and(|min| value < min);
        if over || under {
            violations += 1;
        }
        println!("{}  {}  value {value:.3}  max {:?}  min {:?}", if over || under { "FAIL         " } else { "pass         " }, gate.measure, gate.max, gate.min);
    }
    if violations > 0 {
        return Err(format!("{violations} gate(s) violated"));
    }
    Ok(())
}
