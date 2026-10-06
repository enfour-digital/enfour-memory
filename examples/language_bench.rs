//! Small validator measurement. No database or retrieval model is loaded.
use enfour_memory::{language::Language, store::Remember};
use std::{path::PathBuf, time::Instant};
fn main() -> anyhow::Result<()> {
    let path = PathBuf::from(std::env::var("ENFOUR_LANGUAGE")?);
    let started = Instant::now();
    let mut language = Language::load(&path);
    let note=Remember{scope:"repo:benchmark".into(),key:"compiler".into(),title:"Compiler error".into(),
        content:"Keep the error code unchanged. The server must not remove the source.\n\n~~~text\nE0502: cannot borrow the source; retry_count=3\n~~~".into(),
        kind:"fact".into(),source:"test://benchmark".into(),expected_revision:0,expires_at:None};
    let report = language.require(&note)?;
    let first_ms = started.elapsed().as_secs_f64() * 1000.;
    let mut measurements = Vec::new();
    for cached in [false, true] {
        language.set_cache_enabled(cached);
        language.require(&note)?;
        let mut times = Vec::new();
        for _ in 0..50 {
            let start = Instant::now();
            let got = language.require(&note)?;
            times.push(start.elapsed().as_secs_f64() * 1e6);
            assert_eq!(serde_json::to_value(&report)?, serde_json::to_value(got)?);
        }
        times.sort_by(f64::total_cmp);
        measurements.push(
            serde_json::json!({"cached":cached,"samples":50,"p50_us":times[25],"p95_us":times[47]}),
        );
    }
    let rss = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM:"))
                .map(str::to_string)
        });
    println!(
        "{}",
        serde_json::to_string_pretty(
            &serde_json::json!({"first_report_ms":first_ms,"peak_process_memory":rss,"measurements":measurements,"language":language.info()})
        )?
    );
    Ok(())
}
