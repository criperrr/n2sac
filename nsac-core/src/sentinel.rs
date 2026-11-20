use sha2::{Digest, Sha256};

use crate::filters::normalize_text;
use crate::models::AllYearsResponse;

/// Computes a deterministic SHA-256 hex digest over class-wide M.T. (`class.average_grade`)
/// entries `(school_year, normalized_subject_name, bimester, formatted_mt)`.
///
/// Student-specific fields (`personal.grade`, absences, recovery status, `M.F.`, `F.T.`,
/// `bimesters_metrics`, `YearInfo.status`) are intentionally excluded so any two students
/// in the same class produce the exact same hash.
pub fn compute_class_grade_hash(boletim: &AllYearsResponse) -> String {
    let mut records: Vec<(u16, String, u8, String)> = Vec::new();

    for year_info in &boletim.data {
        for subject in &year_info.grades {
            let subject_norm = normalize_text(&subject.subject_name)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");

            for bimester in &subject.bimesters {
                let mt_canonical = match bimester.class.average_grade {
                    Some(v) if v.is_finite() => {
                        let clean = if v == 0.0 { 0.0 } else { v };
                        format!("{clean:.2}")
                    }
                    _ => "NONE".to_string(),
                };

                records.push((year_info.year, subject_norm.clone(), bimester.bimester, mt_canonical));
            }
        }
    }

    records.sort();

    let mut hasher = Sha256::new();
    hasher.update(b"nsac-sentinel-v1\n");
    hasher.update((records.len() as u64).to_be_bytes());

    for (year, subj, bim, mt) in records {
        hasher.update(year.to_be_bytes());
        hasher.update((subj.len() as u32).to_be_bytes());
        hasher.update(subj.as_bytes());
        hasher.update([bim]);
        hasher.update((mt.len() as u32).to_be_bytes());
        hasher.update(mt.as_bytes());
    }

    hex::encode(hasher.finalize())
}
