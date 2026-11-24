//! Pure-Rust PDF 1.4 generator for student grade reports (`boletim`) built directly
//! from PostgreSQL data. Renders a modern editorial A4 Landscape table mirroring
//! the complete NSAC boletim structure (grouped bimester headers with per-bimester
//! average & absences, M.B. / M.T. / F. / REC pills, and RESULTADO M.F. / F.T.)
//! with upgraded typography, color semantics, and zero UNESP/CTI logos.

use std::fmt::Write as _;

use chrono::{DateTime, FixedOffset, Utc};
use nsac_core::models::{FullGrades, RecoveryStatusCode, YearInfo};

const PAGE_W: f64 = 841.89; // A4 Landscape width in pt
const PAGE_H: f64 = 595.28; // A4 Landscape height in pt
const MARGIN_X: f64 = 28.0;
const CONTENT_W: f64 = PAGE_W - 2.0 * MARGIN_X;

/// Metadata about the student for the PDF header.
#[derive(Debug, Clone)]
pub struct BoletimPdfMeta {
    pub student_name: String,
    pub class_code: String,
    pub generated_at: DateTime<Utc>,
}

#[derive(Clone, Copy)]
struct Rgb(f64, f64, f64);

const C_INK: Rgb = Rgb(0.09, 0.11, 0.14);
const C_MUTED: Rgb = Rgb(0.42, 0.46, 0.52);
const C_SUBTLE: Rgb = Rgb(0.60, 0.64, 0.70);
const C_WHITE: Rgb = Rgb(1.0, 1.0, 1.0);
const C_TBL_GRP_BG: Rgb = Rgb(0.95, 0.96, 0.975);
const C_TBL_SUB_BG: Rgb = Rgb(0.98, 0.985, 0.99);
const C_RESULT_BG: Rgb = Rgb(0.94, 0.955, 0.975);
const C_ZEBRA: Rgb = Rgb(0.982, 0.985, 0.99);
const C_BORDER: Rgb = Rgb(0.84, 0.86, 0.90);
const C_GROUP_DIV: Rgb = Rgb(0.75, 0.78, 0.83);
const C_RED: Rgb = Rgb(0.80, 0.15, 0.15);
const C_TEAL: Rgb = Rgb(0.05, 0.48, 0.43);
const C_BLUE_BADGE: Rgb = Rgb(0.14, 0.46, 0.76);
const C_AMBER_BADGE: Rgb = Rgb(0.85, 0.50, 0.04);
const C_RED_BADGE: Rgb = Rgb(0.82, 0.17, 0.20);

struct BimesterSummary {
    avg_str: String,
    total_absences: u32,
}

struct OverallSummary {
    personal_avg: String,
    class_avg: String,
    total_absences: u32,
    above_six: usize,
    total_subjects: usize,
}

/// Renders a single `YearInfo` grade report into a valid A4 Landscape PDF 1.4 byte vector.
pub fn render_boletim_pdf(year: &YearInfo, meta: &BoletimPdfMeta) -> Vec<u8> {
    let bim_count: u8 = year
        .grades
        .iter()
        .flat_map(|g| g.bimesters.iter().map(|b| b.bimester))
        .max()
        .unwrap_or(4)
        .clamp(2, 4);

    let bim_summaries = compute_bimester_summaries(year, bim_count);
    let overall = compute_overall_summary(year);

    let mut pages: Vec<String> = Vec::new();
    let mut current = String::with_capacity(16384);
    let mut page_num = 1usize;

    let mut y = draw_clean_header(&mut current, year, meta);
    let mut table_top_y = y;
    y = draw_table_header(&mut current, y, bim_count, &bim_summaries);

    let row_h = if year.grades.len() <= 16 { 19.5 } else { 17.5 };
    let bottom_limit = 78.0;

    for (idx, subject) in year.grades.iter().enumerate() {
        if y - row_h < bottom_limit {
            draw_table_frame(&mut current, table_top_y, y, bim_count);
            draw_page_footer(&mut current, page_num);
            pages.push(current);

            page_num += 1;
            current = String::with_capacity(8192);
            y = draw_continuation_header(&mut current, year, meta, page_num);
            table_top_y = y;
            y = draw_table_header(&mut current, y, bim_count, &bim_summaries);
        }

        draw_subject_row(&mut current, y, row_h, idx, subject, bim_count);
        y -= row_h;
    }

    draw_table_frame(&mut current, table_top_y, y, bim_count);

    if y >= 72.0 {
        draw_summary_strip(&mut current, y - 14.0, &overall, meta);
    }
    draw_page_footer(&mut current, page_num);
    pages.push(current);

    assemble_pdf_document(&pages)
}

fn compute_bimester_summaries(year: &YearInfo, bim_count: u8) -> Vec<BimesterSummary> {
    (1..=bim_count)
        .map(|b| {
            let mut sum = 0.0;
            let mut cnt = 0u32;
            let mut abs = 0u32;
            for subj in &year.grades {
                if let Some(bm) = subj.bimesters.iter().find(|x| x.bimester == b) {
                    if let Some(g) = bm.personal.grade {
                        sum += g;
                        cnt += 1;
                    }
                    abs += bm.personal.absences;
                }
            }
            let avg_str = if cnt > 0 {
                fmt_pt_num(sum / f64::from(cnt), 1)
            } else {
                "-".to_string()
            };
            BimesterSummary {
                avg_str,
                total_absences: abs,
            }
        })
        .collect()
}

fn compute_overall_summary(year: &YearInfo) -> OverallSummary {
    let mut sum_p = 0.0;
    let mut cnt_p = 0u32;
    let mut sum_c = 0.0;
    let mut cnt_c = 0u32;
    let mut total_abs = 0u32;
    let mut above_six = 0usize;

    for subj in &year.grades {
        let mut s_sum = 0.0;
        let mut s_cnt = 0u32;
        let mut s_abs = 0u32;
        for b in &subj.bimesters {
            if let Some(g) = b.personal.grade {
                s_sum += g;
                s_cnt += 1;
                sum_p += g;
                cnt_p += 1;
            }
            if let Some(c) = b.class.average_grade {
                sum_c += c;
                cnt_c += 1;
            }
            s_abs += b.personal.absences;
        }
        total_abs += subj.results.total_absences.max(s_abs);
        let mf = subj.results.grade.or_else(|| {
            if s_cnt > 0 {
                Some(s_sum / f64::from(s_cnt))
            } else {
                None
            }
        });
        if mf.is_some_and(|v| v >= 6.0) {
            above_six += 1;
        }
    }

    OverallSummary {
        personal_avg: if cnt_p > 0 {
            fmt_pt_num(sum_p / f64::from(cnt_p), 2)
        } else {
            "-".to_string()
        },
        class_avg: if cnt_c > 0 {
            fmt_pt_num(sum_c / f64::from(cnt_c), 2)
        } else {
            "-".to_string()
        },
        total_absences: total_abs,
        above_six,
        total_subjects: year.grades.len(),
    }
}

fn draw_clean_header(s: &mut String, year: &YearInfo, meta: &BoletimPdfMeta) -> f64 {
    let mut y = PAGE_H - 34.0;

    // Clean white paper typographic header (v1 style, zero dark banner, zero UNESP/CTI logos)
    draw_text_rgb(s, MARGIN_X, y, "F2", 18.0, "n²sac", C_INK);
    draw_text_rgb(
        s,
        MARGIN_X + 60.0,
        y + 5.5,
        "F2",
        10.5,
        "EXTRATO DE BOLETIM ESCOLAR",
        C_INK,
    );
    draw_text_rgb(
        s,
        MARGIN_X + 60.0,
        y - 6.5,
        "F1",
        8.2,
        "Documento gerado a partir dos dados sincronizados (sem valor oficial)",
        C_MUTED,
    );

    let brt = FixedOffset::west_opt(3 * 3600)
        .map(|tz| meta.generated_at.with_timezone(&tz))
        .unwrap_or_else(|| meta.generated_at.fixed_offset());
    let date_label = brt.format("%d/%m/%Y %H:%M").to_string();
    draw_text_right_rgb(s, MARGIN_X + CONTENT_W, y + 4.0, "F1", 8.5, &date_label, C_MUTED);

    y -= 17.0;
    draw_hline_rgb(s, MARGIN_X, MARGIN_X + CONTENT_W, y, 0.75, Rgb(0.28, 0.30, 0.34));

    // Centered document title (NSAC Ficha style)
    y -= 17.0;
    let title = format!("Boletim Individual - Ano Letivo {}", year.year);
    draw_text_center_rgb(s, PAGE_W / 2.0, y, "F2", 12.0, &title, C_INK);

    // Centered 4-column metadata strip (bold label on top, regular value below)
    y -= 20.0;
    let cols = [
        ("Aluno(a)", truncate_chars(&meta.student_name, 46), 0.0, 340.0),
        ("Turma", truncate_chars(&meta.class_code, 20), 340.0, 150.0),
        ("Ano", year.year.to_string(), 490.0, 110.0),
        (
            "Situação",
            truncate_chars(&year.status, 22),
            600.0,
            CONTENT_W - 600.0,
        ),
    ];

    for (label, val, offset, width) in cols {
        let cx = MARGIN_X + offset + width / 2.0;
        draw_text_center_rgb(s, cx, y, "F2", 9.0, label, C_INK);
        draw_text_center_rgb(s, cx, y - 12.5, "F1", 9.0, &val, C_INK);
    }

    y -= 21.0;
    draw_hline_rgb(s, MARGIN_X, MARGIN_X + CONTENT_W, y, 0.45, C_BORDER);
    y - 9.0
}

fn draw_summary_strip(
    s: &mut String,
    y: f64,
    overall: &OverallSummary,
    meta: &BoletimPdfMeta,
) {
    let brt = FixedOffset::west_opt(3 * 3600)
        .map(|tz| meta.generated_at.with_timezone(&tz))
        .unwrap_or_else(|| meta.generated_at.fixed_offset());
    draw_text_rgb(s, MARGIN_X, y, "F2", 8.8, "Resumo:", C_INK);
    let line = format!(
        "Média Geral: {}   |   Média da Turma: {}   |   Total de Faltas: {}   |   Disciplinas >= 6,0: {} de {}   |   Gerado em {}",
        overall.personal_avg,
        overall.class_avg,
        overall.total_absences,
        overall.above_six,
        overall.total_subjects,
        brt.format("%d/%m/%Y às %H:%M")
    );
    draw_text_rgb(s, MARGIN_X + 46.0, y, "F3", 8.4, &line, C_MUTED);
}

fn draw_continuation_header(
    s: &mut String,
    year: &YearInfo,
    meta: &BoletimPdfMeta,
    page_num: usize,
) -> f64 {
    let y = PAGE_H - 34.0;
    let title = format!(
        "n²sac  •  Boletim {} ({})  •  {}  •  Página {}",
        year.year, meta.class_code, meta.student_name, page_num
    );
    draw_text_rgb(s, MARGIN_X, y, "F2", 9.5, &truncate_chars(&title, 110), C_INK);
    y - 12.0
}

struct GridLayout {
    subj_w: f64,
    mb_w: f64,
    mt_w: f64,
    f_w: f64,
    rec_w: f64,
    mf_w: f64,
    ft_w: f64,
}

impl GridLayout {
    fn bim_group_w(&self) -> f64 {
        self.mb_w + self.mt_w + self.f_w + self.rec_w
    }

    fn result_group_w(&self) -> f64 {
        self.mf_w + self.ft_w
    }
}

fn grid_layout(bim_count: u8) -> GridLayout {
    if bim_count <= 2 {
        let mb_w = 56.0;
        let mt_w = 56.0;
        let f_w = 46.0;
        let rec_w = 56.0;
        let mf_w = 54.0;
        let ft_w = 48.0;
        let used = f64::from(bim_count) * (mb_w + mt_w + f_w + rec_w) + mf_w + ft_w;
        GridLayout {
            subj_w: CONTENT_W - used,
            mb_w,
            mt_w,
            f_w,
            rec_w,
            mf_w,
            ft_w,
        }
    } else {
        // 4 bimesters across 785.89 pt:
        // Subject = 165.89, each bimester = 34 + 34 + 26 + 40 = 134 (*4 = 536), result = 44 + 40 = 84
        let mb_w = 34.0;
        let mt_w = 34.0;
        let f_w = 26.0;
        let rec_w = 40.0;
        let mf_w = 44.0;
        let ft_w = 40.0;
        let used = f64::from(bim_count) * (mb_w + mt_w + f_w + rec_w) + mf_w + ft_w;
        GridLayout {
            subj_w: CONTENT_W - used,
            mb_w,
            mt_w,
            f_w,
            rec_w,
            mf_w,
            ft_w,
        }
    }
}

fn draw_table_header(
    s: &mut String,
    y: f64,
    bim_count: u8,
    summaries: &[BimesterSummary],
) -> f64 {
    let lay = grid_layout(bim_count);
    let grp_h = 30.0;
    let sub_h = 19.0;

    // Group row background + RESULTADO accent tint
    draw_rect_fill_rgb(s, MARGIN_X, y - grp_h, CONTENT_W, grp_h, C_TBL_GRP_BG);
    let res_x = MARGIN_X + lay.subj_w + f64::from(bim_count) * lay.bim_group_w();
    draw_rect_fill_rgb(s, res_x, y - grp_h, lay.result_group_w(), grp_h, C_RESULT_BG);

    // Sub-header row background
    draw_rect_fill_rgb(s, MARGIN_X, y - grp_h - sub_h, CONTENT_W, sub_h, C_TBL_SUB_BG);
    draw_rect_fill_rgb(s, res_x, y - grp_h - sub_h, lay.result_group_w(), sub_h, C_RESULT_BG);

    // Group labels
    draw_text_rgb(s, MARGIN_X + 10.0, y - 18.0, "F2", 9.0, "Disciplina", C_INK);

    let mut x = MARGIN_X + lay.subj_w;
    for (idx, sum) in summaries.iter().enumerate() {
        let b = idx + 1;
        let gw = lay.bim_group_w();
        let cx = x + gw / 2.0;
        let title = format!("{b}º BIMESTRE");
        let sub = format!("Média: {}  •  Faltas: {}", sum.avg_str, sum.total_absences);
        draw_text_center_rgb(s, cx, y - 12.5, "F2", 8.4, &title, C_INK);
        draw_text_center_rgb(s, cx, y - 23.5, "F1", 7.1, &sub, C_MUTED);
        x += gw;
    }

    draw_text_center_rgb(
        s,
        res_x + lay.result_group_w() / 2.0,
        y - 17.5,
        "F2",
        8.5,
        "RESULTADO",
        C_INK,
    );

    // Horizontal line between group header and sub-header
    draw_hline_rgb(s, MARGIN_X, MARGIN_X + CONTENT_W, y - grp_h, 0.5, C_BORDER);

    // Sub-column labels (Nome | M.B. M.T. F. REC ... | M.F. F.T.)
    let sub_y = y - grp_h - 12.5;
    draw_text_rgb(s, MARGIN_X + 10.0, sub_y, "F2", 7.8, "Nome", C_MUTED);

    let mut sx = MARGIN_X + lay.subj_w;
    for _ in 0..bim_count {
        draw_text_center_rgb(s, sx + lay.mb_w / 2.0, sub_y, "F2", 7.6, "M.B.", C_INK);
        sx += lay.mb_w;
        draw_text_center_rgb(s, sx + lay.mt_w / 2.0, sub_y, "F2", 7.4, "M.T.", C_MUTED);
        sx += lay.mt_w;
        draw_text_center_rgb(s, sx + lay.f_w / 2.0, sub_y, "F2", 7.4, "F.", C_MUTED);
        sx += lay.f_w;
        draw_text_center_rgb(s, sx + lay.rec_w / 2.0, sub_y, "F2", 7.4, "REC", C_MUTED);
        sx += lay.rec_w;
    }

    draw_text_center_rgb(s, sx + lay.mf_w / 2.0, sub_y, "F2", 7.8, "M.F.", C_INK);
    sx += lay.mf_w;
    draw_text_center_rgb(s, sx + lay.ft_w / 2.0, sub_y, "F2", 7.6, "F.T.", C_MUTED);

    // Bottom header divider
    draw_hline_rgb(
        s,
        MARGIN_X,
        MARGIN_X + CONTENT_W,
        y - grp_h - sub_h,
        0.8,
        C_GROUP_DIV,
    );

    y - grp_h - sub_h
}

fn draw_subject_row(
    s: &mut String,
    y: f64,
    row_h: f64,
    idx: usize,
    subject: &FullGrades,
    bim_count: u8,
) {
    let lay = grid_layout(bim_count);
    let res_x = MARGIN_X + lay.subj_w + f64::from(bim_count) * lay.bim_group_w();

    if idx % 2 == 1 {
        draw_rect_fill_rgb(s, MARGIN_X, y - row_h, CONTENT_W, row_h, C_ZEBRA);
    }
    // Subtle RESULTADO column tint
    let res_tint = if idx % 2 == 1 {
        Rgb(0.94, 0.96, 0.985)
    } else {
        Rgb(0.96, 0.972, 0.99)
    };
    draw_rect_fill_rgb(s, res_x, y - row_h, lay.result_group_w(), row_h, res_tint);

    draw_hline_rgb(s, MARGIN_X, MARGIN_X + CONTENT_W, y - row_h, 0.35, C_BORDER);

    let text_y = y - (row_h * 0.64);
    let max_subj_chars = if bim_count <= 2 { 48 } else { 29 };
    let name = truncate_chars(&subject.subject_name, max_subj_chars);
    draw_text_rgb(s, MARGIN_X + 10.0, text_y, "F1", 8.3, &name, C_INK);

    let mut x = MARGIN_X + lay.subj_w;
    let mut sum_grades = 0.0;
    let mut count_grades = 0u32;
    let mut sum_abs = 0u32;

    for b in 1..=bim_count {
        let bm_opt = subject.bimesters.iter().find(|bm| bm.bimester == b);
        if let Some(bm) = bm_opt {
            // M.B. (with optional '*' if recovered/SAT, red if < 6.0, teal if == 10.0)
            match bm.personal.grade {
                Some(g) => {
                    sum_grades += g;
                    count_grades += 1;
                    let is_rec_star = bm.personal.recovered == Some(true)
                        || bm.personal.recovery_code == Some(RecoveryStatusCode::Sat);
                    let label = if is_rec_star {
                        format!("{}*", fmt_pt_num(g, 1))
                    } else {
                        fmt_pt_num(g, 1)
                    };
                    let (font, color) = if g < 6.0 {
                        ("F2", C_RED)
                    } else if g >= 9.95 {
                        ("F2", C_TEAL)
                    } else {
                        ("F1", C_INK)
                    };
                    draw_text_center_rgb(s, x + lay.mb_w / 2.0, text_y, font, 8.2, &label, color);
                }
                None => {
                    draw_text_center_rgb(s, x + lay.mb_w / 2.0, text_y, "F1", 8.0, "-", C_SUBTLE);
                }
            }
            x += lay.mb_w;

            // M.T.
            match bm.class.average_grade {
                Some(m) => {
                    let color = if m < 6.0 { C_RED } else { C_MUTED };
                    draw_text_center_rgb(
                        s,
                        x + lay.mt_w / 2.0,
                        text_y,
                        "F1",
                        8.0,
                        &fmt_pt_num(m, 1),
                        color,
                    );
                }
                None => {
                    draw_text_center_rgb(s, x + lay.mt_w / 2.0, text_y, "F1", 8.0, "-", C_SUBTLE);
                }
            }
            x += lay.mt_w;

            // F. (Absences)
            sum_abs += bm.personal.absences;
            let abs_color = if bm.personal.absences > 0 {
                C_INK
            } else {
                C_SUBTLE
            };
            draw_text_center_rgb(
                s,
                x + lay.f_w / 2.0,
                text_y,
                "F1",
                8.0,
                &bm.personal.absences.to_string(),
                abs_color,
            );
            x += lay.f_w;

            // REC pill badge (SAT / NC / INS)
            let rec_code = bm
                .personal
                .recovery_code
                .filter(|rc| *rc != RecoveryStatusCode::Nac);
            if let Some(rc) = rec_code {
                let badge_bg = match rc {
                    RecoveryStatusCode::Sat => C_BLUE_BADGE,
                    RecoveryStatusCode::Nc => C_AMBER_BADGE,
                    RecoveryStatusCode::Ins => C_RED_BADGE,
                    RecoveryStatusCode::Nac => C_MUTED,
                };
                let bw = 27.0;
                let bh = 12.5;
                let bx = x + (lay.rec_w - bw) / 2.0;
                let by = y - row_h + (row_h - bh) / 2.0;
                draw_rounded_rect_fill(s, bx, by, bw, bh, 3.5, badge_bg);
                draw_text_center_rgb(
                    s,
                    x + lay.rec_w / 2.0,
                    by + 3.6,
                    "F2",
                    6.8,
                    rc.as_code_str(),
                    C_WHITE,
                );
            } else {
                draw_text_center_rgb(s, x + lay.rec_w / 2.0, text_y, "F1", 8.0, "-", C_SUBTLE);
            }
            x += lay.rec_w;
        } else {
            draw_text_center_rgb(s, x + lay.mb_w / 2.0, text_y, "F1", 8.0, "-", C_SUBTLE);
            x += lay.mb_w;
            draw_text_center_rgb(s, x + lay.mt_w / 2.0, text_y, "F1", 8.0, "-", C_SUBTLE);
            x += lay.mt_w;
            draw_text_center_rgb(s, x + lay.f_w / 2.0, text_y, "F1", 8.0, "-", C_SUBTLE);
            x += lay.f_w;
            draw_text_center_rgb(s, x + lay.rec_w / 2.0, text_y, "F1", 8.0, "-", C_SUBTLE);
            x += lay.rec_w;
        }
    }

    // RESULTADO: M.F.
    let mf = subject.results.grade.or_else(|| {
        if count_grades > 0 {
            Some(sum_grades / f64::from(count_grades))
        } else {
            None
        }
    });
    match mf {
        Some(v) => {
            let color = if v < 6.0 {
                C_RED
            } else if v >= 9.95 {
                C_TEAL
            } else {
                C_INK
            };
            draw_text_center_rgb(
                s,
                x + lay.mf_w / 2.0,
                text_y,
                "F2",
                8.6,
                &fmt_pt_num(v, 1),
                color,
            );
        }
        None => {
            draw_text_center_rgb(s, x + lay.mf_w / 2.0, text_y, "F1", 8.2, "-", C_SUBTLE);
        }
    }
    x += lay.mf_w;

    // RESULTADO: F.T.
    let total_abs = subject.results.total_absences.max(sum_abs);
    draw_text_center_rgb(
        s,
        x + lay.ft_w / 2.0,
        text_y,
        "F1",
        8.2,
        &total_abs.to_string(),
        C_INK,
    );
}

fn draw_table_frame(s: &mut String, top_y: f64, bottom_y: f64, bim_count: u8) {
    let lay = grid_layout(bim_count);
    let h = top_y - bottom_y;
    if h <= 0.0 {
        return;
    }

    // Vertical group dividers between Subject, each Bimester, and RESULTADO
    let mut vx = MARGIN_X + lay.subj_w;
    draw_vline_rgb(s, vx, bottom_y, top_y, 0.55, C_GROUP_DIV);
    for _ in 0..bim_count {
        vx += lay.bim_group_w();
        draw_vline_rgb(s, vx, bottom_y, top_y, 0.55, C_GROUP_DIV);
    }

    // Clean outer rectangle stroke
    draw_rect_stroke_rgb(s, MARGIN_X, bottom_y, CONTENT_W, h, 0.75, C_GROUP_DIV);
}

fn draw_page_footer(s: &mut String, page_num: usize) {
    let y = 26.0;
    draw_hline_rgb(s, MARGIN_X, MARGIN_X + CONTENT_W, y + 12.0, 0.45, C_BORDER);
    draw_text_rgb(
        s,
        MARGIN_X,
        y,
        "F1",
        7.4,
        "Legenda: M.B. = Média Bimestral  •  M.T. = Média da Turma  •  F. = Faltas  •  REC = Recuperação (SAT / INS / NC)  •  M.F. = Média Final  •  F.T. = Faltas Totais",
        C_MUTED,
    );
    let right = format!("n²sac  •  Página {page_num}");
    draw_text_right_rgb(s, MARGIN_X + CONTENT_W, y, "F2", 7.6, &right, C_MUTED);
}

// --- Low-level PDF 1.4 primitives ---

fn fmt_pt_num(val: f64, decimals: usize) -> String {
    format!("{val:.decimals$}").replace('.', ",")
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    let mut iter = s.trim().chars();
    let taken: String = iter.by_ref().take(max_chars).collect();
    if iter.next().is_some() && max_chars > 2 {
        let mut shorter: String = taken.chars().take(max_chars - 2).collect();
        shorter.push_str("...");
        shorter
    } else {
        taken
    }
}

fn approx_text_width(text: &str, font_size: f64, bold: bool) -> f64 {
    let factor = if bold { 0.54 } else { 0.50 };
    let mut units = 0.0;
    for ch in text.chars() {
        units += match ch {
            'i' | 'l' | 'I' | '.' | ',' | ':' | ';' | '!' | '|' | '\'' | ' ' => 0.29,
            'f' | 'j' | 'r' | 't' | '-' | '(' | ')' | '/' | '*' => 0.37,
            'm' | 'w' | 'M' | 'W' => 0.84,
            'A'..='Z' | 'Á' | 'À' | 'Â' | 'Ã' | 'É' | 'Ê' | 'Í' | 'Ó' | 'Ô' | 'Õ' | 'Ú' | 'Ç' => {
                0.68
            }
            _ => factor,
        };
    }
    units * font_size
}

fn pdf_encode_winansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    out.push('(');
    for ch in text.chars() {
        let byte: u8 = match ch {
            '\u{0020}'..='\u{007E}' => ch as u8,
            '€' => 0x80,
            '–' => 0x96,
            '—' => 0x97,
            '•' => 0x95,
            '\u{00A0}'..='\u{00FF}' => ch as u8,
            _ => b'?',
        };
        match byte {
            b'(' | b')' | b'\\' => {
                out.push('\\');
                out.push(byte as char);
            }
            0x20..=0x7E => out.push(byte as char),
            other => {
                let _ = write!(out, "\\{:03o}", other);
            }
        }
    }
    out.push(')');
    out
}

fn draw_text_rgb(s: &mut String, x: f64, y: f64, font: &str, size: f64, text: &str, c: Rgb) {
    let encoded = pdf_encode_winansi(text);
    let _ = writeln!(
        s,
        "BT {:.3} {:.3} {:.3} rg /{font} {size:.1} Tf 1 0 0 1 {x:.2} {y:.2} Tm {encoded} Tj ET",
        c.0, c.1, c.2
    );
}

fn draw_text_center_rgb(
    s: &mut String,
    cx: f64,
    y: f64,
    font: &str,
    size: f64,
    text: &str,
    c: Rgb,
) {
    let w = approx_text_width(text, size, font == "F2");
    draw_text_rgb(s, cx - w / 2.0, y, font, size, text, c);
}

fn draw_text_right_rgb(
    s: &mut String,
    rx: f64,
    y: f64,
    font: &str,
    size: f64,
    text: &str,
    c: Rgb,
) {
    let w = approx_text_width(text, size, font == "F2");
    draw_text_rgb(s, rx - w, y, font, size, text, c);
}

fn draw_hline_rgb(s: &mut String, x1: f64, x2: f64, y: f64, width: f64, c: Rgb) {
    let _ = writeln!(
        s,
        "q {:.3} {:.3} {:.3} RG {width:.2} w {x1:.2} {y:.2} m {x2:.2} {y:.2} l S Q",
        c.0, c.1, c.2
    );
}

fn draw_vline_rgb(s: &mut String, x: f64, y1: f64, y2: f64, width: f64, c: Rgb) {
    let _ = writeln!(
        s,
        "q {:.3} {:.3} {:.3} RG {width:.2} w {x:.2} {y1:.2} m {x:.2} {y2:.2} l S Q",
        c.0, c.1, c.2
    );
}

fn draw_rect_fill_rgb(s: &mut String, x: f64, y: f64, w: f64, h: f64, c: Rgb) {
    let _ = writeln!(
        s,
        "q {:.3} {:.3} {:.3} rg {x:.2} {y:.2} {w:.2} {h:.2} re f Q",
        c.0, c.1, c.2
    );
}

fn draw_rect_stroke_rgb(s: &mut String, x: f64, y: f64, w: f64, h: f64, lw: f64, c: Rgb) {
    let _ = writeln!(
        s,
        "q {:.3} {:.3} {:.3} RG {lw:.2} w {x:.2} {y:.2} {w:.2} {h:.2} re S Q",
        c.0, c.1, c.2
    );
}

/// Draws a filled rounded rectangle using cubic Bézier corner arcs (`k = 0.55228475`).
fn draw_rounded_rect_fill(s: &mut String, x: f64, y: f64, w: f64, h: f64, r: f64, c: Rgb) {
    let r = r.min(w / 2.0).min(h / 2.0);
    let k = r * 0.55228475;
    let x2 = x + w;
    let y2 = y + h;
    let _ = writeln!(
        s,
        "q {:.3} {:.3} {:.3} rg \
         {:.2} {:.2} m \
         {:.2} {:.2} l \
         {:.2} {:.2} {:.2} {:.2} {:.2} {:.2} c \
         {:.2} {:.2} l \
         {:.2} {:.2} {:.2} {:.2} {:.2} {:.2} c \
         {:.2} {:.2} l \
         {:.2} {:.2} {:.2} {:.2} {:.2} {:.2} c \
         {:.2} {:.2} l \
         {:.2} {:.2} {:.2} {:.2} {:.2} {:.2} c \
         f Q",
        c.0,
        c.1,
        c.2,
        x + r,
        y,
        x2 - r,
        y,
        x2 - r + k,
        y,
        x2,
        y + r - k,
        x2,
        y + r,
        x2,
        y2 - r,
        x2,
        y2 - r + k,
        x2 - r + k,
        y2,
        x2 - r,
        y2,
        x + r,
        y2,
        x + r - k,
        y2,
        x,
        y2 - r + k,
        x,
        y2 - r,
        x,
        y + r,
        x,
        y + r - k,
        x + r - k,
        y,
        x + r,
        y
    );
}

fn assemble_pdf_document(page_streams: &[String]) -> Vec<u8> {
    let mut pdf: Vec<u8> = Vec::with_capacity(24576);
    pdf.extend_from_slice(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n");

    let mut offsets: Vec<usize> = Vec::with_capacity(6 + page_streams.len() * 2);
    offsets.push(0);

    let push_obj = |buf: &mut Vec<u8>, offs: &mut Vec<usize>, id: usize, body: &[u8]| {
        offs.push(buf.len());
        let header = format!("{id} 0 obj\n");
        buf.extend_from_slice(header.as_bytes());
        buf.extend_from_slice(body);
        buf.extend_from_slice(b"\nendobj\n");
    };

    push_obj(&mut pdf, &mut offsets, 1, b"<< /Type /Catalog /Pages 2 0 R >>");

    let kids: Vec<String> = (0..page_streams.len())
        .map(|i| format!("{} 0 R", 6 + i * 2))
        .collect();
    let pages_body = format!(
        "<< /Type /Pages /Kids [{}] /Count {} >>",
        kids.join(" "),
        page_streams.len()
    );
    push_obj(&mut pdf, &mut offsets, 2, pages_body.as_bytes());

    push_obj(
        &mut pdf,
        &mut offsets,
        3,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
    );
    push_obj(
        &mut pdf,
        &mut offsets,
        4,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>",
    );
    push_obj(
        &mut pdf,
        &mut offsets,
        5,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Oblique /Encoding /WinAnsiEncoding >>",
    );

    for (i, stream_str) in page_streams.iter().enumerate() {
        let page_id = 6 + i * 2;
        let content_id = 7 + i * 2;
        let page_body = format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {PAGE_W:.2} {PAGE_H:.2}] \
             /Resources << /Font << /F1 3 0 R /F2 4 0 R /F3 5 0 R >> >> \
             /Contents {content_id} 0 R >>"
        );
        push_obj(&mut pdf, &mut offsets, page_id, page_body.as_bytes());

        let stream_bytes = stream_str.as_bytes();
        let mut content_body = Vec::with_capacity(stream_bytes.len() + 64);
        let prefix = format!("<< /Length {} >>\nstream\n", stream_bytes.len());
        content_body.extend_from_slice(prefix.as_bytes());
        content_body.extend_from_slice(stream_bytes);
        content_body.extend_from_slice(b"\nendstream");
        push_obj(&mut pdf, &mut offsets, content_id, &content_body);
    }

    let xref_offset = pdf.len();
    let xref_header = format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len());
    pdf.extend_from_slice(xref_header.as_bytes());
    for off in offsets.iter().skip(1) {
        let entry = format!("{off:010} 00000 n \n");
        pdf.extend_from_slice(entry.as_bytes());
    }

    let trailer = format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
        offsets.len()
    );
    pdf.extend_from_slice(trailer.as_bytes());
    pdf
}

#[cfg(test)]
mod tests {
    use super::*;
    use nsac_core::models::{
        ClassBiInformation, FullGrades, PersonalBiInformation, ResultData, UnifiedBimesterData,
    };

    fn sample_year(subject_count: usize, bimesters_per_subject: u8) -> YearInfo {
        let grades = (0..subject_count)
            .map(|i| FullGrades {
                subject_name: format!("Disciplina {i} — Matemática"),
                results: ResultData {
                    grade: Some(if i == 0 { 5.8 } else { 8.5 }),
                    total_absences: 4,
                },
                bimesters: (1..=bimesters_per_subject)
                    .map(|b| UnifiedBimesterData {
                        bimester: b,
                        personal: PersonalBiInformation {
                            grade: Some(if b == 1 && i == 0 { 5.7 } else { 8.2 }),
                            absences: 1,
                            recovery: b == 1 && i == 0,
                            recovered: if b == 1 && i == 0 { Some(true) } else { None },
                            recovery_code: if b == 1 && i == 0 {
                                Some(RecoveryStatusCode::Sat)
                            } else {
                                None
                            },
                            recovery_message: None,
                        },
                        class: ClassBiInformation {
                            average_grade: Some(7.1),
                        },
                    })
                    .collect(),
            })
            .collect();

        YearInfo {
            title: "INI3".to_string(),
            year: 2026,
            status: "Cursando".to_string(),
            grades,
            bimesters_metrics: Vec::new(),
        }
    }

    #[test]
    fn generates_valid_landscape_pdf_with_badges_and_no_institutional_logos() {
        let year = sample_year(16, 4);
        let meta = BoletimPdfMeta {
            student_name: "Gabriel Camaforte dos Santos".to_string(),
            class_code: "INI3".to_string(),
            generated_at: Utc::now(),
        };

        let bytes = render_boletim_pdf(&year, &meta);
        assert!(bytes.starts_with(b"%PDF-1.4\n"));
        assert!(bytes.ends_with(b"%%EOF\n"));

        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("/MediaBox [0 0 841.89 595.28]"));
        assert!(text.contains("RESULTADO"));
        assert!(text.contains("(SAT)"));
        assert!(text.contains("Gabriel Camaforte dos Santos"));
        assert!(!text.to_uppercase().contains("UNESP"));
        assert!(!text.to_uppercase().contains("UNIVERSIDADE ESTADUAL"));
        assert!(!text.to_uppercase().contains("ISAAC PORTAL"));
    }

    #[test]
    fn paginates_cleanly_for_large_subject_lists_and_supports_two_bimesters() {
        let year_many = sample_year(32, 4);
        let meta = BoletimPdfMeta {
            student_name: "Aluno Teste".to_string(),
            class_code: "INF1".to_string(),
            generated_at: Utc::now(),
        };
        let bytes = render_boletim_pdf(&year_many, &meta);
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("/Count 2"));

        let year_noturno = sample_year(8, 2);
        let bytes_n = render_boletim_pdf(&year_noturno, &meta);
        assert!(bytes_n.starts_with(b"%PDF-1.4\n"));
        assert!(bytes_n.ends_with(b"%%EOF\n"));
    }
}
