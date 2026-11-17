//! Local reproducible evaluator: reference.rttm hypothesis.rttm options.json.
use anyhow::{ensure, Context, Result};
use kikimimic_core::diarization_evaluation::{evaluate, Options};

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    ensure!(
        args.len() == 3,
        "Usage: evaluate_diarization reference.rttm hypothesis.rttm options.json"
    );
    let reference = std::fs::read_to_string(&args[0]).context("Read reference RTTM")?;
    let hypothesis = std::fs::read_to_string(&args[1]).context("Read hypothesis RTTM")?;
    let options: Options = serde_json::from_str(&std::fs::read_to_string(&args[2])?)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&evaluate(&reference, &hypothesis, options)?)?
    );
    Ok(())
}
