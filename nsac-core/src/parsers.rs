use scraper::{ElementRef, Html, Selector};

use crate::error::AppError;
use crate::filters::normalize_text;
use crate::models::{
    AllYearsResponse, AtrasoRecord, BimesterData, ClassBiInformation, FichaAvaliacaoDetail,
    FichaAvaliacaoEntry, FullGrades, HomeInfo, HomeLinkItem, PersonalBiInformation,
    RecoveryMessage, RecoveryStatusCode, ResultData, SaidaRecord, UnifiedBimesterData,
    WarningValue, YearInfo,
};

#[inline]
fn sel(pattern: &str) -> Result<Selector, AppError> {
    Selector::parse(pattern)
        .map_err(|_| AppError::internal(format!("Invalid CSS selector: {pattern}")))
}

fn element_text(el: ElementRef<'_>) -> String {
    el.text().collect::<Vec<_>>().join(" ").trim().to_string()
}

fn round_2dp(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn parse_grade_cell(raw: &str) -> Option<f64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "-" || trimmed == "--" {
        return None;
    }

    let normalized = trimmed.replace(',', ".");
    let mut token = String::new();
    let mut started = false;
    let mut has_dot = false;

    for ch in normalized.chars() {
        if !started {
            if ch.is_ascii_digit() || ch == '-' {
                started = true;
                token.push(ch);
            }
        } else if ch.is_ascii_digit() {
            token.push(ch);
        } else if ch == '.' && !has_dot {
            has_dot = true;
            token.push(ch);
        } else {
            break;
        }
    }

    if token.is_empty() || token == "-" {
        return None;
    }

    token.parse::<f64>().ok().filter(|v| v.is_finite())
}

fn parse_absences_cell(raw: &str) -> u32 {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "-" || trimmed == "--" {
        return 0;
    }

    let digits: String = trimmed.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse::<u32>().unwrap_or(0)
}

fn looks_like_login_doc(doc: &Html) -> bool {
    let Ok(login_sel) = Selector::parse("form[action*=\"/login\"] input[name=\"password\"]") else {
        return false;
    };
    if doc.select(&login_sel).next().is_some() {
        return true;
    }
    let Ok(body_sel) = Selector::parse("body.login-page input[name=\"password\"]") else {
        return false;
    };
    doc.select(&body_sel).next().is_some()
}

pub fn looks_like_login_page(html: &str) -> bool {
    let doc = Html::parse_document(html);
    looks_like_login_doc(&doc)
}

pub fn extract_csrf_token(html: &str) -> Result<String, AppError> {
    let doc = Html::parse_document(html);
    let token_sel = sel("input[name=\"_token\"]")?;

    let token = doc
        .select(&token_sel)
        .filter_map(|el| el.value().attr("value"))
        .map(str::trim)
        .find(|v| !v.is_empty())
        .ok_or_else(|| AppError::nsac_unavailable("Missing CSRF _token in NSAC login page"))?;

    Ok(token.to_string())
}

fn parse_box_header(header_text: &str) -> Result<(String, u16, String), AppError> {
    let (raw_class, raw_year_status) = header_text.split_once('/').ok_or_else(|| {
        AppError::nsac_unavailable("Invalid boletim box header format: missing '/'")
    })?;

    let title = raw_class.trim().to_string();
    if title.is_empty() {
        return Err(AppError::nsac_unavailable(
            "Invalid boletim box header format: empty class code",
        ));
    }

    let rest = raw_year_status.trim();
    let (year_str, status) = match rest.split_once('-') {
        Some((y, s)) => {
            let trimmed_status = s.trim();
            let status_val = if trimmed_status.is_empty() {
                "Cursando.".to_string()
            } else {
                trimmed_status.to_string()
            };
            (y.trim(), status_val)
        }
        None => (rest, "Cursando.".to_string()),
    };

    let year_digits: String = year_str
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    let year = year_digits.parse::<u16>().map_err(|_| {
        AppError::nsac_unavailable("Invalid boletim box header format: invalid year")
    })?;

    Ok((title, year, status))
}

pub fn parse_boletim(html: &str) -> Result<AllYearsResponse, AppError> {
    let doc = Html::parse_document(html);
    if looks_like_login_doc(&doc) {
        return Err(AppError::auth_expired(
            "NSAC returned login page when fetching boletim",
        ));
    }

    let box_sel = sel("div.box.box-primary")?;
    let header_h4_sel = sel("div.box-header h4")?;
    let row_sel = sel("table#notas tbody tr.linha, tr.linha")?;
    let subject_td_sel = sel("td:not(.text-center)")?;
    let span_sel = sel("span")?;
    let center_td_sel = sel("td.text-center")?;

    let boxes: Vec<ElementRef<'_>> = doc.select(&box_sel).collect();
    if boxes.is_empty() {
        let h1_sel = sel("section.content-header h1")?;
        let is_boletim_page = doc
            .select(&h1_sel)
            .any(|el| normalize_text(&element_text(el)).contains("boletim"));
        if is_boletim_page {
            return Ok(AllYearsResponse {
                warning: WarningValue::Bool(false),
                user_current_year: 0,
                data: Vec::new(),
            });
        }
        return Err(AppError::nsac_unavailable(
            "Unexpected boletim HTML structure: no year boxes found",
        ));
    }

    let mut years = Vec::with_capacity(boxes.len());

    // NSAC renders newest year first; reverse for chronological order (oldest -> newest).
    for box_el in boxes.into_iter().rev() {
        let header_el = box_el.select(&header_h4_sel).next().ok_or_else(|| {
            AppError::nsac_unavailable("Boletim year box missing div.box-header h4")
        })?;
        let (title, year, status) = parse_box_header(&element_text(header_el))?;

        let mut sum_user = [0.0_f64; 4];
        let mut count_user = [0_u32; 4];
        let mut sum_class = [0.0_f64; 4];
        let mut count_class = [0_u32; 4];
        let mut bimester_absences = [0_u32; 4];

        let mut grades = Vec::new();

        for row_el in box_el.select(&row_sel) {
            let subject_td = row_el.select(&subject_td_sel).next().ok_or_else(|| {
                AppError::nsac_unavailable("Boletim subject row missing subject cell")
            })?;
            let subject_name = if let Some(sp) = subject_td.select(&span_sel).next() {
                let abbrev = element_text(sp);
                sp.value()
                    .attr("title")
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .unwrap_or(abbrev)
            } else {
                element_text(subject_td)
            };

            let cells: Vec<ElementRef<'_>> = row_el.select(&center_td_sel).collect();
            if cells.len() < 18 {
                return Err(AppError::nsac_unavailable(format!(
                    "Boletim row for '{subject_name}' expected at least 18 cells, got {}",
                    cells.len()
                )));
            }

            let mut bimesters = Vec::with_capacity(4);
            for b_idx in 0..4 {
                // Fresh per-bimester parsing inside loop (fixes PF-EXTRA-02 carry-over).
                let base = b_idx * 4;
                let user_grade = parse_grade_cell(&element_text(cells[base]));
                let class_avg = parse_grade_cell(&element_text(cells[base + 1]));
                let absences = parse_absences_cell(&element_text(cells[base + 2]));

                let rec_cell = cells[base + 3];
                let rec_span = rec_cell.select(&span_sel).last();
                let rec_text = rec_span
                    .map(element_text)
                    .unwrap_or_else(|| element_text(rec_cell));
                let rec_code = RecoveryStatusCode::from_cell_text(&rec_text);
                let recovery = rec_code != RecoveryStatusCode::Nac;

                // When both personal grade (M.B.) and class average (M.T.) are 0.0 (and no recovery occurred),
                // NSAC Online has not posted this bimester yet — do not include unposted 0.0s in bimestersMetrics!
                let is_unposted_zero = !recovery
                    && user_grade.is_some_and(|g| g.abs() <= 1e-9)
                    && class_avg.unwrap_or(0.0).abs() <= 1e-9;

                let title_attr = rec_span
                    .and_then(|s| s.value().attr("title"))
                    .or_else(|| rec_cell.value().attr("title"));
                let rec_message = RecoveryMessage::from_title_or_code(title_attr, rec_code);
                let recovered = if recovery {
                    Some(rec_code == RecoveryStatusCode::Sat || user_grade.is_some_and(|g| g >= 6.0))
                } else {
                    None
                };

                if !is_unposted_zero {
                    if let Some(g) = user_grade {
                        sum_user[b_idx] += g;
                        count_user[b_idx] += 1;
                    }
                    if let Some(c) = class_avg.filter(|&c| c > 0.0 || user_grade.is_some_and(|g| g > 0.0)) {
                        sum_class[b_idx] += c;
                        count_class[b_idx] += 1;
                    }
                }
                bimester_absences[b_idx] += absences;

                bimesters.push(UnifiedBimesterData {
                    bimester: (b_idx + 1) as u8,
                    personal: PersonalBiInformation {
                        grade: user_grade,
                        absences,
                        recovery,
                        recovered,
                        recovery_code: Some(rec_code),
                        recovery_message: Some(rec_message),
                    },
                    class: ClassBiInformation {
                        average_grade: class_avg,
                    },
                });
            }

            // Final result columns M.F. (16) and F.T. (17) — PF-05 safe on unposted '-'.
            let final_grade = parse_grade_cell(&element_text(cells[16]));
            let final_absences = parse_absences_cell(&element_text(cells[17]));

            grades.push(FullGrades {
                subject_name,
                results: ResultData {
                    grade: final_grade,
                    total_absences: final_absences,
                },
                bimesters,
            });
        }

        let bimesters_metrics = (0..4)
            .map(|i| BimesterData {
                user_average: if count_user[i] > 0 {
                    round_2dp(sum_user[i] / f64::from(count_user[i]))
                } else {
                    0.0
                },
                class_average: if count_class[i] > 0 {
                    round_2dp(sum_class[i] / f64::from(count_class[i]))
                } else {
                    0.0
                },
                total_absences: bimester_absences[i],
            })
            .collect();

        years.push(YearInfo {
            title,
            year,
            status,
            grades,
            bimesters_metrics,
        });
    }

    Ok(AllYearsResponse {
        warning: WarningValue::Bool(false),
        user_current_year: years.len(),
        data: years,
    })
}

pub fn parse_home(html: &str) -> Result<HomeInfo, AppError> {
    let doc = Html::parse_document(html);
    if looks_like_login_doc(&doc) {
        return Err(AppError::auth_expired(
            "NSAC returned login page when fetching home",
        ));
    }

    let header_sel = sel("section.content-header h1")?;
    let sidebar_sel = sel("aside.main-sidebar ul.sidebar-menu")?;
    if doc.select(&header_sel).next().is_none() && doc.select(&sidebar_sel).next().is_none() {
        return Err(AppError::nsac_unavailable(
            "Unexpected home HTML structure: missing header and sidebar",
        ));
    }

    let date_sel = sel("section.content-header h1 small")?;
    let portal_date = doc
        .select(&date_sel)
        .next()
        .map(element_text)
        .filter(|s| !s.is_empty());

    let route_sel = sel("aside.main-sidebar ul.sidebar-menu li a[href]")?;
    let mut available_routes = Vec::new();
    for link in doc.select(&route_sel) {
        if let Some(href) = link.value().attr("href") {
            let trimmed = href.trim();
            if trimmed.is_empty() || trimmed == "#" {
                continue;
            }
            let route = url::Url::parse(trimmed)
                .map(|u| u.path().to_string())
                .unwrap_or_else(|_| trimmed.to_string());
            if !available_routes.contains(&route) {
                available_routes.push(route);
            }
        }
    }

    let anchor_sel = sel("section.content a[href]")?;
    let info_box_sel = sel("div.info-box")?;
    let content_info_box_sel = sel("section.content div.info-box")?;
    let text_sel = sel(".info-box-text")?;
    let number_sel = sel(".info-box-number")?;

    let mut info_links = Vec::new();
    for anchor in doc.select(&anchor_sel) {
        if anchor.select(&info_box_sel).next().is_none() {
            continue;
        }
        let Some(href) = anchor.value().attr("href").map(str::trim) else {
            continue;
        };
        if href.is_empty() {
            continue;
        }
        let title = anchor
            .select(&text_sel)
            .next()
            .map(element_text)
            .unwrap_or_default();
        let description = anchor
            .select(&number_sel)
            .next()
            .map(element_text)
            .unwrap_or_default();
        info_links.push(HomeLinkItem {
            title,
            description,
            url: href.to_string(),
        });
    }

    if info_links.is_empty() {
        for box_el in doc.select(&content_info_box_sel) {
            let title = box_el
                .select(&text_sel)
                .next()
                .map(element_text)
                .unwrap_or_default();
            let description = box_el
                .select(&number_sel)
                .next()
                .map(element_text)
                .unwrap_or_default();
            if title.is_empty() && description.is_empty() {
                continue;
            }
            info_links.push(HomeLinkItem {
                title,
                description,
                url: String::new(),
            });
        }
    }

    Ok(HomeInfo {
        portal_date,
        available_routes,
        info_links,
    })
}

struct ParsedTable {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
}

fn parse_table_rows(doc: &Html) -> Result<Option<ParsedTable>, AppError> {
    let content_sel = sel("section.content")?;
    let content = doc.select(&content_sel).next().ok_or_else(|| {
        AppError::nsac_unavailable("Unexpected NSAC page structure: missing section.content")
    })?;

    let table_sel = sel("table")?;
    let Some(table) = content.select(&table_sel).next() else {
        return Ok(None);
    };

    let th_sel = sel("thead tr th")?;
    let headers: Vec<String> = table
        .select(&th_sel)
        .map(|th| normalize_text(&element_text(th)))
        .collect();

    let tr_sel = sel("tbody tr")?;
    let td_sel = sel("td")?;
    let mut rows = Vec::new();

    for tr in table.select(&tr_sel) {
        let cells: Vec<ElementRef<'_>> = tr.select(&td_sel).collect();
        if cells.is_empty() {
            continue;
        }
        if cells.len() == 1 && cells[0].value().attr("colspan").is_some() {
            continue;
        }
        let raw_cols: Vec<String> = cells.into_iter().map(element_text).collect();
        if raw_cols.iter().all(|c| c.is_empty()) {
            continue;
        }
        rows.push(raw_cols);
    }

    Ok(Some(ParsedTable { headers, rows }))
}

fn find_column_index(headers: &[String], keywords: &[&str]) -> Option<usize> {
    headers
        .iter()
        .position(|h| keywords.iter().any(|kw| h.contains(kw)))
}

fn opt_col(cols: &[String], idx: Option<usize>, fallback: usize) -> Option<String> {
    let target = idx.unwrap_or(fallback);
    cols.get(target)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && s != "-")
}

pub fn parse_atrasos(html: &str) -> Result<Vec<AtrasoRecord>, AppError> {
    let doc = Html::parse_document(html);
    if looks_like_login_doc(&doc) {
        return Err(AppError::auth_expired(
            "NSAC returned login page when fetching atrasos",
        ));
    }

    let Some(ParsedTable { headers, rows }) = parse_table_rows(&doc)? else {
        return Ok(Vec::new());
    };

    let date_idx = find_column_index(&headers, &["data", "dia"]);
    let time_idx = find_column_index(&headers, &["hora", "horario"]);
    let subject_idx = find_column_index(&headers, &["disciplina", "materia", "aula"]);
    let reason_idx = find_column_index(&headers, &["motivo", "justificativa", "obs"]);

    let mut records = Vec::with_capacity(rows.len());
    for raw_columns in rows {
        let date = opt_col(&raw_columns, date_idx, 0).unwrap_or_default();
        let time = opt_col(&raw_columns, time_idx, 1);
        let subject = subject_idx.and_then(|i| opt_col(&raw_columns, Some(i), i));
        let reason = match reason_idx {
            Some(i) => opt_col(&raw_columns, Some(i), i),
            None => {
                if subject_idx.is_none() {
                    opt_col(&raw_columns, None, 2)
                } else {
                    opt_col(&raw_columns, None, 3)
                }
            }
        };

        records.push(AtrasoRecord {
            date,
            time,
            subject,
            reason,
            raw_columns,
        });
    }

    Ok(records)
}

pub fn parse_saidas(html: &str) -> Result<Vec<SaidaRecord>, AppError> {
    let doc = Html::parse_document(html);
    if looks_like_login_doc(&doc) {
        return Err(AppError::auth_expired(
            "NSAC returned login page when fetching saidas",
        ));
    }

    let Some(ParsedTable { headers, rows }) = parse_table_rows(&doc)? else {
        return Ok(Vec::new());
    };

    let date_idx = find_column_index(&headers, &["data", "dia"]);
    let time_idx = find_column_index(&headers, &["hora", "horario"]);
    let reason_idx = find_column_index(&headers, &["motivo", "justificativa", "obs"]);
    let auth_idx = find_column_index(&headers, &["autoriz", "responsavel"]);

    let mut records = Vec::with_capacity(rows.len());
    for raw_columns in rows {
        let date = opt_col(&raw_columns, date_idx, 0).unwrap_or_default();
        let time = opt_col(&raw_columns, time_idx, 1);
        let reason = opt_col(&raw_columns, reason_idx, 2);
        let authorized_by = auth_idx.and_then(|i| opt_col(&raw_columns, Some(i), i));

        records.push(SaidaRecord {
            date,
            time,
            reason,
            authorized_by,
            raw_columns,
        });
    }

    Ok(records)
}

pub fn parse_ficha_avaliacao_list(html: &str) -> Result<Vec<FichaAvaliacaoEntry>, AppError> {
    let doc = Html::parse_document(html);
    if looks_like_login_doc(&doc) {
        return Err(AppError::auth_expired(
            "NSAC returned login page when fetching ficha-avaliacao",
        ));
    }

    // Intentionally ignore <title> because NSAC has "<title>NSac Online - Saídas</title>" typo.
    let h1_sel = sel("section.content-header h1")?;
    let box_table_sel = sel("div.box.box-default table")?;
    let has_avalia_header = doc
        .select(&h1_sel)
        .any(|el| normalize_text(&element_text(el)).contains("avalia"));
    let has_box_table = doc.select(&box_table_sel).next().is_some();

    if !has_avalia_header && !has_box_table {
        return Err(AppError::nsac_unavailable(
            "Unexpected ficha-avaliacao HTML structure",
        ));
    }

    let box_title_sel = sel("h3.box-title")?;
    let school_year = doc.select(&box_title_sel).next().and_then(|el| {
        element_text(el)
            .split_whitespace()
            .rev()
            .find_map(|tok| {
                let digits: String = tok.chars().filter(|c| c.is_ascii_digit()).collect();
                if digits.len() == 4 {
                    digits.parse::<u16>().ok()
                } else {
                    None
                }
            })
    });

    let row_sel = sel("div.box.box-default table tbody tr, section.content table tbody tr")?;
    let td_sel = sel("td")?;
    let link_sel = sel("a[href]")?;

    let mut entries = Vec::new();
    for tr in doc.select(&row_sel) {
        let cells: Vec<ElementRef<'_>> = tr.select(&td_sel).collect();
        if cells.len() < 3 {
            continue;
        }

        let bimester_text = element_text(cells[0]);
        let bimester_digits: String = bimester_text
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        let Ok(bimester) = bimester_digits.parse::<u8>() else {
            continue;
        };

        let last_viewed_raw = element_text(cells[1]);
        let last_viewed = if last_viewed_raw.is_empty() || last_viewed_raw == "-" {
            None
        } else {
            Some(last_viewed_raw)
        };

        if let Some(anchor) = cells[2].select(&link_sel).next() {
            let detail_url = anchor
                .value()
                .attr("href")
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            let detail_id = detail_url
                .as_deref()
                .and_then(|u| u.trim_end_matches('/').rsplit('/').next())
                .and_then(|seg| seg.parse::<u8>().ok())
                .or(Some(bimester));
            let anchor_text = element_text(anchor);
            let status_text = if anchor_text.is_empty() {
                element_text(cells[2])
            } else {
                anchor_text
            };

            entries.push(FichaAvaliacaoEntry {
                school_year,
                bimester,
                last_viewed,
                status_text,
                available: true,
                detail_id,
                detail_url,
            });
        } else {
            entries.push(FichaAvaliacaoEntry {
                school_year,
                bimester,
                last_viewed,
                status_text: element_text(cells[2]),
                available: false,
                detail_id: None,
                detail_url: None,
            });
        }
    }

    Ok(entries)
}

pub fn parse_ficha_avaliacao_detail(
    html: &str,
    bimester: u8,
) -> Result<FichaAvaliacaoDetail, AppError> {
    let doc = Html::parse_document(html);
    if looks_like_login_doc(&doc) {
        return Err(AppError::auth_expired(
            "NSAC returned login page when fetching ficha-avaliacao detail",
        ));
    }

    let content_sel = sel("section.content")?;
    let content = doc.select(&content_sel).next().ok_or_else(|| {
        AppError::nsac_unavailable("Unexpected ficha-avaliacao detail HTML structure")
    })?;

    let box_title_sel = sel("h3.box-title")?;
    let h1_sel = sel("section.content-header h1")?;
    let title = doc
        .select(&box_title_sel)
        .next()
        .or_else(|| doc.select(&h1_sel).next())
        .map(element_text)
        .filter(|s| !s.is_empty());

    let table_sel = sel("table")?;
    let tr_sel = sel("tr")?;
    let cell_sel = sel("th, td")?;

    let mut tables = Vec::new();
    for table in content.select(&table_sel) {
        let mut parsed_rows = Vec::new();
        for tr in table.select(&tr_sel) {
            let cells: Vec<String> = tr.select(&cell_sel).map(element_text).collect();
            if !cells.is_empty() {
                parsed_rows.push(cells);
            }
        }
        if !parsed_rows.is_empty() {
            tables.push(parsed_rows);
        }
    }

    let note_sel = sel("div.box-body p, div.callout p, ul li")?;
    let notes: Vec<String> = content
        .select(&note_sel)
        .map(element_text)
        .filter(|s| !s.is_empty())
        .collect();

    Ok(FichaAvaliacaoDetail {
        bimester,
        title,
        tables,
        notes,
    })
}
