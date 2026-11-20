use nsac_core::error::ErrorCode;
use nsac_core::models::{RecoveryMessage, RecoveryStatusCode};
use nsac_core::parsers::{
    extract_csrf_token, parse_atrasos, parse_boletim, parse_ficha_avaliacao_detail,
    parse_ficha_avaliacao_list, parse_home, parse_saidas,
};

#[test]
fn parses_login_csrf_token() {
    let html = include_str!("fixtures/login_page.html");
    let token = extract_csrf_token(html).unwrap();
    assert_eq!(token, "SYNTHETIC_CSRF_TOKEN_40_CHARACTERS_LONG1");
}

#[test]
fn parses_authenticated_home_route_and_links() {
    let html = include_str!("fixtures/home_authenticated.html");
    let home = parse_home(html).unwrap();

    assert_eq!(home.portal_date.as_deref(), Some("19/11/2026"));
    assert_eq!(
        home.available_routes,
        vec![
            "/nsac/aluno/boletim",
            "/nsac/aluno/atrasos",
            "/nsac/aluno/saidas",
            "/nsac/aluno/ficha-avaliacao",
            "/nsac/change_password",
        ]
    );
    assert_eq!(home.info_links.len(), 3);
    assert_eq!(home.info_links[0].title, "Facebook");
    assert_eq!(home.info_links[1].url, "http://www.cti.feb.unesp.br/");
}

#[test]
fn parses_noturno_inf1_boletim_with_pf05_and_pf_extra_02_fixes() {
    let html = include_str!("fixtures/boletim_noturno_inf1.html");
    let resp = parse_boletim(html).unwrap();

    assert_eq!(resp.user_current_year, 1);
    assert_eq!(resp.data.len(), 1);

    let year = &resp.data[0];
    assert_eq!(year.class_code(), "INF1");
    assert_eq!(year.year, 2026);
    assert_eq!(year.status, "Cursando.");
    assert_eq!(year.grades.len(), 2);

    let poo = &year.grades[0];
    assert_eq!(poo.subject_name, "Programação Orientada a Objetos");
    // PF-05: M.F. and F.T. are "-" -> None and 0 without panic/error
    assert_eq!(poo.results.grade, None);
    assert_eq!(poo.results.total_absences, 0);

    // Bimester 1 posted (8.5), Bimester 2 unposted ("-") -> PF-EXTRA-02 no carry-over!
    assert_eq!(poo.bimesters[0].personal.grade, Some(8.5));
    assert_eq!(poo.bimesters[0].class.average_grade, Some(7.8));
    assert_eq!(
        poo.bimesters[0].personal.recovery_code,
        Some(RecoveryStatusCode::Nac)
    );
    assert_eq!(
        poo.bimesters[0].personal.recovery_message,
        Some(RecoveryMessage::NaoAconteceu)
    );
    assert!(!poo.bimesters[0].personal.recovery);

    assert_eq!(poo.bimesters[1].personal.grade, None);
    assert_eq!(poo.bimesters[1].class.average_grade, None);
    assert_eq!(poo.bimesters[1].personal.absences, 0);

    // Bimester 3 SAT
    assert_eq!(poo.bimesters[2].personal.grade, Some(5.5));
    assert!(poo.bimesters[2].personal.recovery);
    assert_eq!(poo.bimesters[2].personal.recovered, Some(true));
    assert_eq!(
        poo.bimesters[2].personal.recovery_code,
        Some(RecoveryStatusCode::Sat)
    );
    assert_eq!(
        poo.bimesters[2].personal.recovery_message,
        Some(RecoveryMessage::Satisfatorio)
    );

    // Bimester 4 renders 0.0 -> must be Some(0.0), distinct from None
    assert_eq!(poo.bimesters[3].personal.grade, Some(0.0));
    assert_eq!(poo.bimesters[3].class.average_grade, Some(0.0));

    let bd = &year.grades[1];
    assert_eq!(bd.subject_name, "Banco de Dados I");
    assert_eq!(bd.results.grade, None);
    assert_eq!(bd.results.total_absences, 6);
    assert_eq!(
        bd.bimesters[0].personal.recovery_code,
        Some(RecoveryStatusCode::Ins)
    );
    assert_eq!(bd.bimesters[0].personal.recovered, Some(false));
    assert_eq!(
        bd.bimesters[1].personal.recovery_code,
        Some(RecoveryStatusCode::Nc)
    );
    assert_eq!(
        bd.bimesters[1].personal.recovery_message,
        Some(RecoveryMessage::NaoCompareceu)
    );
}

#[test]
fn parses_diurno_multiyear_in_chronological_order() {
    let html = include_str!("fixtures/boletim_diurno_multiyear.html");
    let resp = parse_boletim(html).unwrap();

    assert_eq!(resp.user_current_year, 3);
    assert_eq!(resp.current_class_code(), Some("INI3A"));
    assert_eq!(resp.data.len(), 3);

    assert_eq!(resp.data[0].title, "INI1A");
    assert_eq!(resp.data[0].year, 2024);
    assert_eq!(resp.data[0].status, "Aprovado");

    assert_eq!(resp.data[1].title, "INI2A");
    assert_eq!(resp.data[1].year, 2025);
    assert_eq!(resp.data[1].status, "Aprovado");

    assert_eq!(resp.data[2].title, "INI3A");
    assert_eq!(resp.data[2].year, 2026);
    assert_eq!(resp.data[2].status, "Cursando.");

    // Lowercase "Não aconteceu" and uppercase "Não Aconteceu" both map to NaoAconteceu
    assert_eq!(
        resp.data[2].grades[0].bimesters[0].personal.recovery_message,
        Some(RecoveryMessage::NaoAconteceu)
    );
    assert_eq!(
        resp.data[2].grades[0].bimesters[1].personal.recovery_message,
        Some(RecoveryMessage::NaoAconteceu)
    );
}

#[test]
fn parses_empty_and_populated_atrasos_and_saidas() {
    let atrasos_empty = parse_atrasos(include_str!("fixtures/atrasos_empty.html")).unwrap();
    assert!(atrasos_empty.is_empty());

    let atrasos = parse_atrasos(include_str!("fixtures/atrasos_populated.html")).unwrap();
    assert_eq!(atrasos.len(), 2);
    assert_eq!(atrasos[0].date, "10/03/2026");
    assert_eq!(atrasos[0].time.as_deref(), Some("07:25"));
    assert_eq!(atrasos[0].subject.as_deref(), Some("Matemática"));
    assert_eq!(
        atrasos[0].reason.as_deref(),
        Some("Transporte público atrasado")
    );

    let saidas_empty = parse_saidas(include_str!("fixtures/saidas_empty.html")).unwrap();
    assert!(saidas_empty.is_empty());

    let saidas = parse_saidas(include_str!("fixtures/saidas_populated.html")).unwrap();
    assert_eq!(saidas.len(), 2);
    assert_eq!(saidas[0].date, "12/05/2026");
    assert_eq!(saidas[0].time.as_deref(), Some("10:30"));
    assert_eq!(saidas[0].reason.as_deref(), Some("Consulta médica"));
    assert_eq!(
        saidas[0].authorized_by.as_deref(),
        Some("Secretaria Escolar")
    );
}

#[test]
fn parses_ficha_avaliacao_list_ignoring_title_typo_and_detail() {
    let list_html = include_str!("fixtures/ficha_avaliacao_list.html");
    let entries = parse_ficha_avaliacao_list(list_html).unwrap();

    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].school_year, Some(2026));
    assert_eq!(entries[0].bimester, 1);
    assert!(!entries[0].available);
    assert_eq!(entries[0].detail_id, None);
    assert_eq!(entries[0].last_viewed, None);

    assert_eq!(entries[1].bimester, 2);
    assert!(entries[1].available);
    assert_eq!(entries[1].detail_id, Some(2));
    assert_eq!(
        entries[1].detail_url.as_deref(),
        Some("http://200.145.153.1/nsac/aluno/ficha-avaliacao/2")
    );
    assert_eq!(entries[1].last_viewed.as_deref(), Some("15/07/2026 14:20"));
    assert_eq!(entries[1].status_text, "Visualizar");

    assert_eq!(entries[2].bimester, 3);
    assert!(!entries[2].available);

    let detail_html = include_str!("fixtures/ficha_avaliacao_detail.html");
    let detail = parse_ficha_avaliacao_detail(detail_html, 2).unwrap();
    assert_eq!(detail.bimester, 2);
    assert_eq!(
        detail.title.as_deref(),
        Some("Ficha de Avaliação - 2º Bimestre / 2026")
    );
    assert_eq!(detail.tables.len(), 1);
    assert_eq!(detail.tables[0].len(), 2);
    assert_eq!(detail.tables[0][1][0], "Banco de Dados I");
    assert_eq!(
        detail.notes,
        vec!["Comparecer à orientação educacional conforme cronograma."]
    );
}

#[test]
fn parsers_return_auth_expired_when_given_login_html() {
    let login_html = include_str!("fixtures/login_page.html");

    assert_eq!(
        parse_home(login_html).unwrap_err().code,
        ErrorCode::NsacAuthExpired
    );
    assert_eq!(
        parse_boletim(login_html).unwrap_err().code,
        ErrorCode::NsacAuthExpired
    );
    assert_eq!(
        parse_atrasos(login_html).unwrap_err().code,
        ErrorCode::NsacAuthExpired
    );
    assert_eq!(
        parse_saidas(login_html).unwrap_err().code,
        ErrorCode::NsacAuthExpired
    );
    assert_eq!(
        parse_ficha_avaliacao_list(login_html).unwrap_err().code,
        ErrorCode::NsacAuthExpired
    );
    assert_eq!(
        parse_ficha_avaliacao_detail(login_html, 1)
            .unwrap_err()
            .code,
        ErrorCode::NsacAuthExpired
    );
}
