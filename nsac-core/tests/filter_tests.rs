use nsac_core::error::ErrorCode;
use nsac_core::filters::{BooleanFilter, NumberFilter, QueryFilter, StringFilter, filter_query};
use nsac_core::models::AllYearsResponse;
use nsac_core::parsers::parse_boletim;

fn sample_multiyear() -> AllYearsResponse {
    parse_boletim(include_str!("fixtures/boletim_diurno_multiyear.html")).unwrap()
}

fn sample_noturno() -> AllYearsResponse {
    parse_boletim(include_str!("fixtures/boletim_noturno_inf1.html")).unwrap()
}

#[test]
fn pf01_class_average_reads_query_class_average_and_compares_class_grade() {
    let data = sample_noturno();
    // In INF1 fixture:
    // POO B1: personal=8.5, class=7.8; B3: personal=5.5, class=5.9; B4: personal=0.0, class=0.0
    // BD1 B1: personal=4.5, class=6.2; B2: personal=5.0, class=6.0; B3: personal=7.5, class=7.0
    // Filter classAverage[gt]=7.0 should match ONLY POO B1 (class=7.8), NOT BD1 B3 (personal=7.5, class=7.0)!
    let filter = QueryFilter::from_query_pairs([("classAverage[gt]", "7.0")]).unwrap();
    let res = filter_query(&data, &filter).unwrap();

    assert_eq!(res.data.len(), 1);
    assert_eq!(res.data[0].grades.len(), 1);
    assert_eq!(
        res.data[0].grades[0].subject_name,
        "Programação Orientada a Objetos"
    );
    assert_eq!(res.data[0].grades[0].bimesters.len(), 1);
    assert_eq!(res.data[0].grades[0].bimesters[0].bimester, 1);
}

#[test]
fn pf02_filter_query_never_mutates_cached_source_data() {
    let original = sample_multiyear();
    let snapshot = original.clone();

    let f1 = QueryFilter::from_query_pairs([("schoolYear", "2026")]).unwrap();
    let f2 = QueryFilter::from_query_pairs([("grade[gte]", "8.0"), ("targetBimester", "2")]).unwrap();

    let r1 = filter_query(&original, &f1).unwrap();
    let r2 = filter_query(&original, &f2).unwrap();

    assert_ne!(r1.data.len(), original.data.len());
    assert_ne!(r2.data.len(), original.data.len());
    assert_eq!(original, snapshot);
}

#[test]
fn pf03_accent_and_case_insensitive_matching_on_both_target_and_filter() {
    let data = sample_multiyear();

    for query_val in ["educacao", "EDUCACAO", "Educação", "educação física"] {
        let filter = QueryFilter {
            subject_name: Some(StringFilter {
                contains: Some(vec![query_val.to_string()]),
                ..Default::default()
            }),
            ..Default::default()
        };
        let res = filter_query(&data, &filter).unwrap();
        assert_eq!(res.data.len(), 1);
        assert_eq!(res.data[0].grades[0].subject_name, "Educação Física");
    }

    // Comma-separated OR list via HTTP query pairs
    let multi = QueryFilter::from_query_pairs([("subjectName[contains]", "mat,fis")]).unwrap();
    let res_multi = filter_query(&data, &multi).unwrap();
    assert_eq!(res_multi.data.len(), 3);
}

#[test]
fn pf_extra_01_boolean_filter_eq_and_neq() {
    let data = sample_noturno();

    let rec_true = QueryFilter {
        is_recovery: Some(BooleanFilter {
            eq: Some(true),
            neq: None,
        }),
        ..Default::default()
    };
    let res_true = filter_query(&data, &rec_true).unwrap();
    assert_eq!(res_true.data[0].grades[0].bimesters.len(), 1); // POO B3 (SAT)
    assert_eq!(res_true.data[0].grades[1].bimesters.len(), 2); // BD1 B1 (INS), B2 (NC)

    let rec_neq_true = QueryFilter::from_query_pairs([("isRecovery[neq]", "1")]).unwrap();
    let res_false = filter_query(&data, &rec_neq_true).unwrap();
    assert_eq!(res_false.data[0].grades[0].bimesters.len(), 3); // POO B1, B2, B4
    assert_eq!(res_false.data[0].grades[1].bimesters.len(), 2); // BD1 B3, B4
}

#[test]
fn pf_extra_03_filters_by_year_and_bimester_values_and_prunes_empty() {
    let data = sample_multiyear();

    let by_year = QueryFilter::from_query_pairs([("schoolYear", "2024")]).unwrap();
    let res_year = filter_query(&data, &by_year).unwrap();
    assert_eq!(res_year.data.len(), 1);
    assert_eq!(res_year.data[0].year, 2024);
    assert_eq!(res_year.data[0].title, "INI1A");

    let by_bim = QueryFilter::from_query_pairs([("targetBimester", "3")]).unwrap();
    let res_bim = filter_query(&data, &by_bim).unwrap();
    for year in &res_bim.data {
        for subj in &year.grades {
            assert_eq!(subj.bimesters.len(), 1);
            assert_eq!(subj.bimesters[0].bimester, 3);
        }
    }

    // Filter matching nothing prunes all years cleanly
    let impossible = QueryFilter::from_query_pairs([("grade[gt]", "10.0")]).unwrap();
    let res_empty = filter_query(&data, &impossible).unwrap();
    assert!(res_empty.data.is_empty());
}

#[test]
fn unposted_none_grades_never_match_numeric_filters() {
    let data = sample_noturno();
    // POO B2 has grade = None; B4 has grade = Some(0.0).
    let filter = QueryFilter::from_query_pairs([("grade[lt]", "5.0")]).unwrap();
    let res = filter_query(&data, &filter).unwrap();
    let poo_bims: Vec<u8> = res.data[0].grades[0]
        .bimesters
        .iter()
        .map(|b| b.bimester)
        .collect();
    assert_eq!(poo_bims, vec![4]);
}

#[test]
fn invalid_query_parameters_return_invalid_params() {
    let bad_queries = [
        ("grade[gt]", "abc"),
        ("classAverage[eq]", "NaN"),
        ("isRecovery", "maybe"),
        ("grade[contains]", "5"),
        ("subjectName", ""),
        ("subjectName[eq]", "Mat,,Fis"),
        ("unknownField", "1"),
        ("grade[gt", "5"),
    ];

    for (k, v) in bad_queries {
        let err = QueryFilter::from_query_pairs([(k, v)]).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidParams, "key={k} val={v}");
    }

    let bad_nf = QueryFilter {
        grade: Some(NumberFilter {
            eq: Some(vec![f64::NAN]),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert_eq!(
        filter_query(&sample_noturno(), &bad_nf).unwrap_err().code,
        ErrorCode::InvalidParams
    );
}
