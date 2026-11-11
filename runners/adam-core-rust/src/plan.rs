//! Plan rows: which rows run, the self-perturber rule, and the adam-core orbit
//! a row's initial condition becomes.
//!
//! Plan conventions (shared with every runner): `ic_pos_au` / `ic_vel_au_d` are
//! ICRF (J2000 equatorial), SSB-centred, au and au/day at `epoch_mjd_tdb`;
//! `t_mjd_tdb` is the target epoch; `ic_a1..ic_a3`, `ic_g_*` and
//! `ic_non_grav_dt` are the Marsden non-gravitational terms.

use adam_core::coords::{
    CoordinateBatch, DataFrame, NonGravitationalParametersBatch, ObjectId, OrbitBatch, OrbitId,
    OriginArray, OriginId, TimeArray, TimeScale,
};
use serde_json::Value;
use std::collections::BTreeSet;

/// The sixteen SB441-N16 perturbers of `sb441-n16.bsp`, by asteroid number.
pub const SB441_N16: [i64; 16] = [1, 2, 3, 4, 7, 10, 15, 16, 31, 52, 65, 87, 88, 107, 511, 704];

pub const FORCE_MODEL: &str = "planets+SB441-n16 (ASSIST_FORCES_DEFAULT)";

/// The reason a self-perturber row is not run, in adam_assist's terms.
pub const SELF_PERTURBER_UNSUPPORTED: &str = "self-perturber: adam_assist 0.7.0 integrates the \
    sixteen SB441-N16 perturbers of sb441-n16.bsp with ASSIST_FORCES_DEFAULT hard-coded \
    (private configure_forces; IntegratorConfig carries only dt/min_dt/epsilon/adaptive_mode), \
    so it cannot leave the object out of its own forces";

/// Why adam_assist does not run this row, or None. Decided from the row's
/// `excluded_perturbers_naif` (NAIF 2000000+n for SB441-N16 body n), never from
/// the object's name or population.
pub fn self_perturber_reason(row: &Value) -> Option<&'static str> {
    let excluded = row["excluded_perturbers_naif"].as_array()?;
    let ids: BTreeSet<i64> = SB441_N16.iter().map(|n| 2_000_000 + n).collect();
    excluded
        .iter()
        .filter_map(Value::as_i64)
        .any(|id| ids.contains(&id))
        .then_some(SELF_PERTURBER_UNSUPPORTED)
}

pub fn f64_field(row: &Value, key: &str) -> Option<f64> {
    row[key].as_f64()
}

fn vec3(row: &Value, key: &str) -> Result<[f64; 3], String> {
    let a = row[key]
        .as_array()
        .filter(|a| a.len() == 3)
        .ok_or_else(|| format!("plan row lacks a 3-vector {key}"))?;
    let mut out = [0.0; 3];
    for (o, v) in out.iter_mut().zip(a) {
        *o = v.as_f64().ok_or_else(|| format!("{key} is not numeric"))?;
    }
    Ok(out)
}

/// adam-core non-gravitational parameters for a row, and the reason the row's
/// non-gravitational model is incomplete in adam-core (`nongrav dt`).
///
/// `NonGravitationalParametersBatch` carries A1/A2/A3 and the g(r) terms (ALN,
/// R0, NM, NN, NK) but no Marsden time delay, so a row with a non-zero
/// `ic_non_grav_dt` runs without it and says so. A row with A-terms and no g(r)
/// takes the standard asteroid law g = 1/r² (alpha 1, r0 1, m 2, n 0, k 0), which
/// is the definition of that law, as the Python channel does.
pub fn nongrav(
    row: &Value,
) -> (
    Option<NonGravitationalParametersBatch>,
    Option<&'static str>,
) {
    let a = |k| f64_field(row, k).unwrap_or(0.0);
    let (a1, a2, a3) = (a("ic_a1"), a("ic_a2"), a("ic_a3"));
    if a1 == 0.0 && a2 == 0.0 && a3 == 0.0 {
        return (None, None);
    }
    let g = [
        f64_field(row, "ic_g_alpha"),
        f64_field(row, "ic_g_r0"),
        f64_field(row, "ic_g_m"),
        f64_field(row, "ic_g_n"),
        f64_field(row, "ic_g_k"),
    ];
    let [aln, r0, nm, nn, nk] = if g.iter().all(Option::is_none) {
        [Some(1.0), Some(1.0), Some(2.0), Some(0.0), Some(0.0)]
    } else {
        g
    };
    let ng = NonGravitationalParametersBatch {
        source: vec![None],
        a1: vec![Some(a1)],
        a2: vec![Some(a2)],
        a3: vec![Some(a3)],
        aln: vec![aln],
        nk: vec![nk],
        nm: vec![nm],
        nn: vec![nn],
        r0: vec![r0],
    };
    let dt = f64_field(row, "ic_non_grav_dt").unwrap_or(0.0);
    (Some(ng), (dt != 0.0).then_some("nongrav dt"))
}

/// The row's initial condition as a one-row adam-core orbit (SSB, equatorial,
/// TDB), with its non-gravitational terms, and the non-grav reason if any.
pub fn plan_orbit(row: &Value) -> Result<(OrbitBatch, Option<&'static str>), String> {
    let p = vec3(row, "ic_pos_au")?;
    let v = vec3(row, "ic_vel_au_d")?;
    let epoch = f64_field(row, "epoch_mjd_tdb").ok_or("plan row lacks epoch_mjd_tdb")?;
    let times = TimeArray::from_mjd(TimeScale::Tdb, &[epoch]).map_err(|e| e.to_string())?;
    let coords = CoordinateBatch::cartesian(
        vec![[p[0], p[1], p[2], v[0], v[1], v[2]]],
        DataFrame::Equatorial,
        OriginArray::repeat(OriginId::SolarSystemBarycenter, 1),
        Some(times),
        None,
    )
    .map_err(|e| e.to_string())?;
    let name = row["object"]
        .as_str()
        .unwrap_or("empyrean_test")
        .to_string();
    let orbits = OrbitBatch::new(
        vec![OrbitId(name.clone())],
        vec![Some(ObjectId(name))],
        coords,
    )
    .map_err(|e| e.to_string())?;
    let (ng, reason) = nongrav(row);
    let orbits = match ng {
        Some(ng) => orbits
            .with_non_gravitational_parameters(ng)
            .map_err(|e| e.to_string())?,
        None => orbits,
    };
    Ok((orbits, reason))
}

/// The plan rows of one axis this runner computes, deduplicated as the Python
/// channel deduplicates them: propagation by (object, dt), ephemeris by
/// (object, dt, observer), OD by object (optical only).
pub fn rows_for<'a>(plan: &'a [Value], axis: &str) -> Vec<&'a Value> {
    let mut seen = BTreeSet::new();
    plan.iter()
        .filter(|r| r["test_type"].as_str() == Some(axis))
        .filter(|r| axis == "orbit_determination" || !r["ic_pos_au"].is_null())
        .filter(|r| {
            let key = match axis {
                "propagation" => format!("{}|{}", r["object"], r["dt_days"]),
                "ephemeris" => format!("{}|{}|{}", r["object"], r["dt_days"], r["observer"]),
                _ => r["object"].to_string(),
            };
            seen.insert(key)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_self_perturber_rule_reads_excluded_perturbers_naif_not_the_name() {
        let vesta = json!({"object": "Vesta", "population": "Self-Perturber",
                           "excluded_perturbers_naif": [2000004]});
        assert_eq!(
            self_perturber_reason(&vesta),
            Some(SELF_PERTURBER_UNSUPPORTED)
        );
        // The name alone is not the rule: Vesta without the field runs.
        assert_eq!(self_perturber_reason(&json!({"object": "Vesta"})), None);
        // A non-SB441-N16 body (2000005, Astraea) is not a self-perturber here.
        assert_eq!(
            self_perturber_reason(&json!({"object": "X", "excluded_perturbers_naif": [2000005]})),
            None
        );
        // Any of the sixteen counts, whatever the object is called.
        assert!(
            self_perturber_reason(
                &json!({"object": "Eros", "excluded_perturbers_naif": [2000704]})
            )
            .is_some()
        );
    }

    #[test]
    fn nongrav_maps_the_plan_terms_and_names_the_missing_dt() {
        let row = json!({"ic_a1": 1e-9, "ic_a2": -2e-10, "ic_a3": 0.0, "ic_g_alpha": 0.1112620426,
                         "ic_g_r0": 2.808, "ic_g_m": 2.15, "ic_g_n": 5.093, "ic_g_k": 4.6142,
                         "ic_non_grav_dt": 3.0});
        let (ng, why) = nongrav(&row);
        let ng = ng.unwrap();
        assert_eq!(
            (
                ng.a1[0], ng.a2[0], ng.aln[0], ng.r0[0], ng.nm[0], ng.nn[0], ng.nk[0]
            ),
            (
                Some(1e-9),
                Some(-2e-10),
                Some(0.1112620426),
                Some(2.808),
                Some(2.15),
                Some(5.093),
                Some(4.6142)
            )
        );
        assert_eq!(why, Some("nongrav dt"));
        // A-terms with no g(r): the standard asteroid law, and no DT reason.
        let (ng, why) = nongrav(&json!({"ic_a2": -2.9e-14}));
        let ng = ng.unwrap();
        assert_eq!(
            (ng.aln[0], ng.r0[0], ng.nm[0], ng.nn[0], ng.nk[0]),
            (Some(1.0), Some(1.0), Some(2.0), Some(0.0), Some(0.0))
        );
        assert_eq!(why, None);
        assert!(nongrav(&json!({"ic_a1": 0.0})).0.is_none());
    }

    #[test]
    fn rows_are_deduplicated_per_axis_as_the_python_channel_does() {
        let plan = vec![
            json!({"test_type": "propagation", "object": "A", "dt_days": 30.0, "ic_pos_au": [1,0,0]}),
            json!({"test_type": "propagation", "object": "A", "dt_days": 30.0, "ic_pos_au": [1,0,0]}),
            json!({"test_type": "propagation", "object": "A", "dt_days": 60.0, "ic_pos_au": [1,0,0]}),
            json!({"test_type": "propagation", "object": "B", "dt_days": 30.0, "ic_pos_au": null}),
            json!({"test_type": "ephemeris", "object": "A", "dt_days": 30.0, "observer": "W84", "ic_pos_au": [1,0,0]}),
            json!({"test_type": "ephemeris", "object": "A", "dt_days": 30.0, "observer": "I41", "ic_pos_au": [1,0,0]}),
            json!({"test_type": "orbit_determination", "object": "A"}),
            json!({"test_type": "orbit_determination", "object": "A"}),
            json!({"test_type": "orbit_determination_radar", "object": "A"}),
        ];
        assert_eq!(rows_for(&plan, "propagation").len(), 2);
        assert_eq!(rows_for(&plan, "ephemeris").len(), 2);
        assert_eq!(rows_for(&plan, "orbit_determination").len(), 1);
    }

    #[test]
    fn the_plan_orbit_is_ssb_equatorial_tdb_with_its_nongrav() {
        let row = json!({"object": "Apophis", "ic_pos_au": [-0.9, 0.5, 0.18],
                         "ic_vel_au_d": [-0.008, -0.011, -0.004], "epoch_mjd_tdb": 61200.0,
                         "ic_a2": -2.9e-14});
        let (o, why) = plan_orbit(&row).unwrap();
        assert_eq!(o.coordinates.frame, DataFrame::Equatorial);
        assert_eq!(
            o.coordinates.origins.origins[0],
            OriginId::SolarSystemBarycenter
        );
        let t = o.coordinates.times.as_ref().unwrap();
        assert_eq!((t.scale, t.epochs[0].mjd()), (TimeScale::Tdb, 61200.0));
        assert!(o.non_gravitational_parameters.is_some());
        assert_eq!(why, None);
        assert!(plan_orbit(&json!({"object": "X"})).is_err());
    }
}
