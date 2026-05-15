//! One-off binary: read a captured-orbits jsonl, run the comparison
//! kernel, print comparisons to stdout. Smoke-test for Phase D.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).expect("usage: run_compare <path>");
    let s = std::fs::read_to_string(&path)?;
    let mut captured: Vec<empyrean_validation::schema::CapturedOrbit> = Vec::new();
    for line in s.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        captured.push(serde_json::from_str(line)?);
    }
    let comps = empyrean_validation::orbit_compare::compare_orbits(&captured, 1.0);
    for c in &comps {
        println!("{}", serde_json::to_string_pretty(c)?);
    }
    Ok(())
}
