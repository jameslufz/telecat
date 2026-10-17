// Shared by scenario.rs and the headless memory benchmark.
use telecat_grid::*;
const TH: [&str; 6] = [
    "ธนาคารกรุงเทพ",
    "ธนาคารกสิกรไทย",
    "ธนาคารกรุงไทย",
    "ธนาคารไทยพาณิชย์",
    "ธนาคารกรุงศรีอยุธยา",
    "ธนาคารทดสอบ",
];
pub fn fixture(name: &str) -> (GridModel, f32, f32) {
    let mut m = match name {
        "grid-07" | "grid-51" => {
            let names = [
                "id",
                "bank_code",
                "bank_name_th",
                "bank_name_en",
                "bank_acronym",
                "logo",
                "country_code",
                "is_disabled",
                "created_at",
                "swift_code",
            ];
            let en = [
                "BANGKOK EDITED",
                "Kasikornbank Public Company Limited",
                "Krungthai Bank Public Company Limited",
                "The Siam Commercial Bank",
                "BANK OF AYUDHYA",
                "Test Bank",
            ];
            let codes = ["002", "004", "006", "014", "025", "777"];
            let acronyms = ["BBL", "KBANK", "KTB", "SCB", "BAY", "TST"];
            let mut cols: Vec<_> = names.into_iter().map(Column::new).collect();
            for (i, c) in cols.iter_mut().enumerate() {
                c.numeric = i == 0 || i == 7;
                c.editable = i != 5;
            }
            let rows = (0..6)
                .map(|i| {
                    vec![
                        Some(if i == 5 {
                            "7".into()
                        } else {
                            (i + 1).to_string()
                        }),
                        Some(codes[i].into()),
                        Some(TH[i].into()),
                        Some(en[i].into()),
                        Some(acronyms[i].into()),
                        None,
                        Some("TH".into()),
                        Some("0".into()),
                        Some(
                            if i == 5 {
                                "2026-10-17 00:38:40"
                            } else {
                                "2026-10-17 00:35:38"
                            }
                            .into(),
                        ),
                        None,
                    ]
                })
                .collect();
            let mut m = GridModel::new(cols, rows);
            // Measured column boundaries in the 2x reference: 628,748,960,1292,1898,2154.
            m.widths = vec![60., 106., 166., 303., 128., 75., 128., 121., 166., 131.];
            if name == "grid-51" {
                // The reference has no modified cell in the visible, horizontally scrolled region.
                m.edits.rows.insert(0, RowState::Modified);
                m.edits.modified.insert(Cell { row: 0, col: 1 });
                m.edits.rows.insert(5, RowState::Deleted);
                let mut row = m.rows.get(5).unwrap().to_owned();
                for col in [0, 5, 7, 8, 9] {
                    row[col] = None;
                    m.edits.defaults.insert(Cell { row: 6, col });
                }
                m.rows.push(row);
                m.edits.rows.insert(6, RowState::New);
                m.selection = Selection::single(6);
                m.selected_column = Some(6);
                m.scroll_x = 526.5;
            }
            m
        }
        "grid-06" => {
            let mut m = GridModel::new(
                vec![Column::new("bank_name_th")],
                TH.iter().map(|v| vec![Some((*v).into())]).collect(),
            );
            m.widths = vec![166.];
            m.selection = Selection::single(0);
            m
        }
        "grid-22" => {
            let mut cols: Vec<_> = ["id", "bank_id", "amount", "note"]
                .into_iter()
                .map(Column::new)
                .collect();
            for (i, c) in cols.iter_mut().enumerate() {
                c.numeric = i < 3;
                c.editable = true;
            }
            GridModel::new(
                cols,
                (1..=500)
                    .map(|r| {
                        vec![
                            Some(r.to_string()),
                            Some((r % 3 + 1).to_string()),
                            Some(format!("{:.2}", r as f64 * 1.5)),
                            Some(format!("note {r}")),
                        ]
                    })
                    .collect(),
            )
        }
        "grid-100k" => large(100000),
        "grid-empty" => {
            let mut m = GridModel::new(["id", "name", "value"].map(Column::new).into(), vec![]);
            m.options.empty_text = "No rows".into();
            m
        }
        _ => panic!(
            "unknown scenario {name}; use grid-07, grid-51, grid-06, grid-22, grid-100k or grid-empty"
        ),
    };
    m.options.hooks = Hooks {
        sort: name != "grid-06",
        edit: name != "grid-06",
        delete: true,
        insert: true,
        duplicate: true,
        paste: true,
        detail: true,
        quick_look: true,
        popup: true,
        ..Default::default()
    };
    let h = if name == "grid-06" { 233.5 } else { 511. };
    m.viewport = Rect {
        x: 0.,
        y: 0.,
        width: 848.,
        height: h,
    };
    m.clamp_scroll();
    (m, 848., h)
}
pub fn large(n: usize) -> GridModel {
    let columns = (0..30)
        .map(|c| {
            let mut col = Column::new(format!("column_{c:02}"));
            col.numeric = c % 3 == 0;
            col.editable = true;
            col
        })
        .collect();
    let rows = (0..n).map(|r| {
        (0..30)
            .map(|c| {
                if c == 29 && r % 7 == 0 {
                    None
                } else {
                    Some(match c % 3 {
                        0 => (r * 30 + c).to_string(),
                        1 => format!("ธนาคาร {r}"),
                        _ => format!("row {r} col {c}"),
                    })
                }
            })
            .collect()
    });
    GridModel::from_rows(columns, rows)
}
