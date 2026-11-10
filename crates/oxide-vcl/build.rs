//! Records the build date for the help screen, and sets `cfg(local_evidence)`
//! when the unpublished original-binary observations are present under the
//! repository's `local/evidence` (tests that need them are ignored without it).
//!
//! The date is UTC. `SOURCE_DATE_EPOCH` overrides the clock, for reproducible
//! builds.

use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    let oracle = "../../local/evidence/oracle";
    println!("cargo::rustc-check-cfg=cfg(local_evidence)");
    println!("cargo::rerun-if-changed={oracle}");
    if std::path::Path::new(oracle).is_dir() {
        println!("cargo::rustc-cfg=local_evidence");
    }

    println!("cargo::rerun-if-env-changed=SOURCE_DATE_EPOCH");
    let seconds = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs())
        });
    let (year, month, day) = civil_date((seconds / 86_400) as i64);
    println!("cargo::rustc-env=VCL_BUILD_DATE={year:04}-{month:02}-{day:02}");
}

/// Proleptic Gregorian date of a day count since 1970-01-01 (Howard
/// Hinnant's `civil_from_days`).
fn civil_date(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    } as u32;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}
