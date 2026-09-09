//! Calibration-only stdin/stdout adapter. Not a public assertion-score command.
use std::io::{self, Read};
use supercov_engine::asserted_coverage::{Facts, join, summary};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    let facts: Facts = serde_json::from_str(&input)?;
    if facts.schema != 1 {
        return Err("unsupported assertion facts schema".into());
    }
    let resolutions = join(&facts);
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "summary": summary(&facts.sites, &resolutions), "resolutions": resolutions,
        }))?
    );
    Ok(())
}
