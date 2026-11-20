use nsac_core::models::{AllYearsResponse, RecoveryMessage, RecoveryStatusCode};
use nsac_core::parsers::parse_boletim;
use nsac_core::sentinel::compute_class_grade_hash;

fn sample_inf1() -> AllYearsResponse {
    parse_boletim(include_str!("fixtures/boletim_noturno_inf1.html")).unwrap()
}

#[test]
fn identical_hash_for_two_students_in_same_class_with_different_personal_grades() {
    let student_a = sample_inf1();
    let mut student_b = student_a.clone();

    // Alter every student-specific field on student_b
    student_b.data[0].status = "Aprovado".to_string();
    for subj in &mut student_b.data[0].grades {
        subj.results.grade = Some(9.5);
        subj.results.total_absences = 18;
        for bim in &mut subj.bimesters {
            bim.personal.grade = Some(10.0);
            bim.personal.absences = 5;
            bim.personal.recovery = true;
            bim.personal.recovered = Some(true);
            bim.personal.recovery_code = Some(RecoveryStatusCode::Sat);
            bim.personal.recovery_message = Some(RecoveryMessage::Satisfatorio);
        }
    }
    for metric in &mut student_b.data[0].bimesters_metrics {
        metric.user_average = 10.0;
        metric.total_absences = 20;
    }

    assert_eq!(
        compute_class_grade_hash(&student_a),
        compute_class_grade_hash(&student_b)
    );
}

#[test]
fn changing_class_average_or_none_to_zero_changes_hash() {
    let base = sample_inf1();
    let hash_base = compute_class_grade_hash(&base);
    assert_eq!(hash_base.len(), 64);

    // Changing M.T. from 7.8 to 7.9 changes hash
    let mut changed_mt = base.clone();
    changed_mt.data[0].grades[0].bimesters[0]
        .class
        .average_grade = Some(7.9);
    assert_ne!(hash_base, compute_class_grade_hash(&changed_mt));

    // Changing M.T. from None (B2) to Some(0.0) changes hash
    let mut none_to_zero = base.clone();
    none_to_zero.data[0].grades[0].bimesters[1]
        .class
        .average_grade = Some(0.0);
    assert_ne!(hash_base, compute_class_grade_hash(&none_to_zero));
}

#[test]
fn canonicalization_ignores_subject_order_whitespace_accents_and_negative_zero() {
    let base = sample_inf1();
    let mut reordered = base.clone();

    reordered.data[0].grades.reverse();
    reordered.data[0].grades[1].subject_name =
        "  Programacao   ORIENTADA  a   Objetos ".to_string();
    reordered.data[0].grades[1].bimesters[3]
        .class
        .average_grade = Some(-0.0);

    assert_eq!(
        compute_class_grade_hash(&base),
        compute_class_grade_hash(&reordered)
    );
}
