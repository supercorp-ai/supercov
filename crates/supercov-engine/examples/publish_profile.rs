//! Development tool: publish a downloaded run's evidence again into a scratch
//! root, timing each part, so publication can be profiled away from CI.
//!
//! Usage: `publish_profile ROOT RUN_DIRECTORY [COUNT]` -- ROOT gets
//! `.supercov/runs/<id>`; RUN_DIRECTORY holds `run.json` and `evidence.raw.gz`.

use std::{path::PathBuf, time::Instant};

use supercov_engine::{lifecycle::publish_run, run_store::RunMetadata};

fn main() {
    let mut args = std::env::args_os().skip(1).map(PathBuf::from);
    let root = args.next().expect("ROOT");
    let run = args.next().expect("RUN_DIRECTORY");
    let times = args
        .next()
        .map_or(1, |count| count.to_string_lossy().parse().expect("COUNT"));
    let text = std::fs::read_to_string(run.join("run.json")).expect("run.json");
    let metadata: RunMetadata = serde_json::from_str(&text).expect("run metadata");
    let destination = root.join(".supercov/runs").join(&metadata.id);
    for _ in 0..times {
        let _ = std::fs::remove_dir_all(&destination);
        let _ = std::fs::remove_dir_all(root.join(".supercov/work"));
        let started = Instant::now();
        publish_run(&root, &metadata, &run.join("evidence.raw.gz")).expect("published");
        eprintln!(
            "publication {:.1}ms",
            started.elapsed().as_secs_f64() * 1000.0
        );
    }
}
