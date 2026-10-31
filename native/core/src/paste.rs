//! Paste to Selected Cells (⇧⌘V): where each copied value lands. The plan is all or nothing:
//! if any cell it would write can't be edited, nothing is staged.
use crate::changes::Cell;

/// Where the paste goes, in the order the grid shows rows and columns. Indices are the staged
/// rows' and the data columns' own (never the display positions).
pub struct PasteTarget<'a> {
    /// The selected rows, top to bottom.
    pub rows: &'a [usize],
    /// The selected columns, left to right.
    pub columns: &'a [usize],
    /// Every row of the grid, top to bottom: a paste from one selected row continues down.
    pub all_rows: &'a [usize],
    /// Every visible column, left to right: a paste from one selected column continues right.
    pub all_columns: &'a [usize],
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteRefusal {
    NothingSelected,
    NothingCopied,
    NotRectangular,
    CellNotEditable,
    DeletedRow,
    /// The copied block (rows × columns) doesn't fit the selection or the grid from the first
    /// selected cell: some values would be cut off, the rest shifted against their columns.
    DoesNotFit {
        rows: usize,
        columns: usize,
    },
}
impl PasteRefusal {
    /// TablePlus's wording where it has one.
    pub fn message(self) -> String {
        match self {
            Self::NothingSelected => "Select the cells to paste into first.".into(),
            Self::NothingCopied => "The clipboard has no text to paste.".into(),
            Self::NotRectangular => {
                "The copied values must form a rectangle: every copied row needs the same number of values.".into()
            }
            Self::CellNotEditable => "A selected cell cannot be edited".into(),
            Self::DeletedRow => "Deleted rows cannot be edited".into(),
            Self::DoesNotFit { rows, columns } => format!(
                "The copied values don't fit here: {rows} × {columns} values need that many rows and shown columns from the first selected cell (or a selection of that size). Nothing was pasted."
            ),
        }
    }
}
/// The cells a paste writes: (row, column, value).
#[derive(Debug, Clone, PartialEq)]
pub struct PastePlan {
    pub cells: Vec<(usize, usize, Cell)>,
}
/// Lays `copied` (rows of tab-separated values) over the selection.
/// - One copied value fills every selected cell.
/// - Otherwise the values start at the selection's first cell. Along each direction where more
///   than one row (or column) is selected they fill only the selection, and cells of the
///   selection beyond the copied values are skipped; where one is selected they continue
///   through the following rows (or columns) of the grid.
/// - A block that doesn't fit (more values than selected or remaining rows / columns) is
///   refused whole: a cut-off block would put the rest against the wrong columns.
///
/// `NULL` pastes SQL NULL, as ⌘V does; `DEFAULT` the column default, for new rows only.
pub fn plan_paste(
    copied: &[Vec<String>],
    target: &PasteTarget,
    editable: impl Fn(usize, usize) -> bool,
    deleted: impl Fn(usize) -> bool,
    is_new: impl Fn(usize) -> bool,
) -> Result<PastePlan, PasteRefusal> {
    if target.rows.is_empty() || target.columns.is_empty() {
        return Err(PasteRefusal::NothingSelected);
    }
    if copied.is_empty() {
        return Err(PasteRefusal::NothingCopied);
    }
    let width = copied[0].len();
    if width == 0 || copied.iter().any(|r| r.len() != width) {
        return Err(PasteRefusal::NotRectangular);
    }
    let lane = |selected: &[usize], all: &[usize], wanted: usize| -> Vec<usize> {
        if selected.len() > 1 {
            selected.iter().copied().take(wanted).collect()
        } else {
            let start = all
                .iter()
                .position(|x| *x == selected[0])
                .unwrap_or(all.len());
            all.iter().copied().skip(start).take(wanted).collect()
        }
    };
    let single = copied.len() == 1 && width == 1;
    let (rows, columns) = if single {
        (target.rows.to_vec(), target.columns.to_vec())
    } else {
        let rows = lane(target.rows, target.all_rows, copied.len());
        let columns = lane(target.columns, target.all_columns, width);
        if rows.len() < copied.len() || columns.len() < width {
            return Err(PasteRefusal::DoesNotFit {
                rows: copied.len(),
                columns: width,
            });
        }
        (rows, columns)
    };
    let mut cells = Vec::with_capacity(rows.len() * columns.len());
    for (i, &row) in rows.iter().enumerate() {
        if deleted(row) {
            return Err(PasteRefusal::DeletedRow);
        }
        for (j, &col) in columns.iter().enumerate() {
            if !editable(row, col) {
                return Err(PasteRefusal::CellNotEditable);
            }
            let text = if single { &copied[0][0] } else { &copied[i][j] };
            let value = match text.as_str() {
                "NULL" => Cell::Null,
                "DEFAULT" if is_new(row) => Cell::unset(),
                _ => Cell::Text(text.clone()),
            };
            cells.push((row, col, value));
        }
    }
    Ok(PastePlan { cells })
}

/// ⌘V (paste rows): copied rows as whole table rows. A row copied as the grid shows it (one
/// value per shown column, in display order, as ⌘C writes it) maps back by `shown`; a row with
/// a value for every column of the table maps in table order. Columns it doesn't cover keep
/// their default. Any other width can't be placed without guessing, so it is refused.
pub fn rows_by_layout(
    copied: &[Vec<String>],
    shown: &[usize],
    columns: usize,
) -> Result<Vec<Vec<Cell>>, String> {
    copied
        .iter()
        .map(|row| {
            let targets: Vec<usize> = if row.len() == shown.len() {
                shown.to_vec()
            } else if row.len() == columns {
                (0..columns).collect()
            } else {
                return Err(format!(
                    "Cannot paste rows: a copied row has {} values, but the table shows {} columns (it has {}). Copy whole rows with ⌘C, or paste into cells with ⇧⌘V.",
                    row.len(),
                    shown.len(),
                    columns
                ));
            };
            let mut values = vec![Cell::unset(); columns];
            for (value, &col) in row.iter().zip(&targets) {
                if let Some(slot) = values.get_mut(col) {
                    *slot = Cell::Text(value.clone());
                }
            }
            Ok(values)
        })
        .collect()
}
/// Where copied columns go in a table with `columns`, by name: Telecat knows the names of the
/// columns it copied (see `telecat_grid::remember_copy`), so values follow their columns
/// whatever the layouts of the two grids. Refused, naming the column, when one of them is
/// missing or ambiguous here.
pub fn columns_by_name(
    copied: &[String],
    columns: &[String],
    source: &str,
) -> Result<Vec<usize>, String> {
    let from = if source.is_empty() {
        String::new()
    } else {
        format!(" (from {source})")
    };
    let mut seen = std::collections::BTreeSet::new();
    copied
        .iter()
        .map(|name| {
            if !seen.insert(name.as_str()) {
                return Err(format!(
                    "Cannot paste: the copied values{from} have two columns named {name}, so they can't be matched by name. Nothing was pasted."
                ));
            }
            let mut found = columns.iter().enumerate().filter(|(_, c)| *c == name).map(|(i, _)| i);
            match (found.next(), found.next()) {
                (Some(i), None) => Ok(i),
                (None, _) => Err(format!(
                    "Cannot paste: the copied values{from} include the column {name}, which isn't here. Nothing was pasted."
                )),
                (Some(_), Some(_)) => Err(format!(
                    "Cannot paste: there are two columns named {name} here, so the copied values{from} can't be matched by name. Nothing was pasted."
                )),
            }
        })
        .collect()
}
/// ⌘V (paste rows) of rows Telecat copied: each value into the column of its name; columns not
/// copied keep their default.
pub fn rows_by_names(
    copied: &[Vec<String>],
    names: &[String],
    columns: &[String],
    source: &str,
) -> Result<Vec<Vec<Cell>>, String> {
    let targets = columns_by_name(names, columns, source)?;
    copied
        .iter()
        .map(|row| {
            if row.len() != targets.len() {
                return Err(
                    "Cannot paste rows: the copied rows don't match the copied columns.".to_owned(),
                );
            }
            let mut values = vec![Cell::unset(); columns.len()];
            for (value, &col) in row.iter().zip(&targets) {
                values[col] = Cell::Text(value.clone());
            }
            Ok(values)
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    fn grid(rows: usize, cols: usize) -> (Vec<usize>, Vec<usize>) {
        ((0..rows).collect(), (0..cols).collect())
    }
    fn copied(text: &[&[&str]]) -> Vec<Vec<String>> {
        text.iter()
            .map(|r| r.iter().map(|s| s.to_string()).collect())
            .collect()
    }
    fn plan(
        copied: &[Vec<String>],
        rows: &[usize],
        cols: &[usize],
    ) -> Result<PastePlan, PasteRefusal> {
        let (all_rows, all_columns) = grid(4, 3);
        plan_paste(
            copied,
            &PasteTarget {
                rows,
                columns: cols,
                all_rows: &all_rows,
                all_columns: &all_columns,
            },
            |_, c| c != 2,
            |r| r == 3,
            |r| r == 1,
        )
    }
    fn text(v: &str) -> Cell {
        Cell::Text(v.into())
    }
    #[test]
    fn one_value_fills_every_selected_cell() {
        let p = plan(&copied(&[&["x"]]), &[0, 2], &[0, 1]).unwrap();
        assert_eq!(
            p.cells,
            [
                (0, 0, text("x")),
                (0, 1, text("x")),
                (2, 0, text("x")),
                (2, 1, text("x"))
            ]
        );
    }
    #[test]
    fn a_block_continues_from_one_cell_and_is_clipped_to_a_larger_selection() {
        let block = copied(&[&["a", "b"], &["c", "d"]]);
        let p = plan(&block, &[1], &[0]).unwrap();
        assert_eq!(
            p.cells,
            [
                (1, 0, text("a")),
                (1, 1, text("b")),
                (2, 0, text("c")),
                (2, 1, text("d"))
            ]
        );
        // Selected cells beyond the copied block are skipped.
        let p = plan(&block, &[0, 1, 2], &[0, 1]).unwrap();
        assert_eq!(p.cells.len(), 4);
        assert!(p.cells.iter().all(|c| c.0 != 2));
        // Only the selection is written along a direction with several selected rows…
        let p = plan(&copied(&[&["a"], &["b"]]), &[0, 2], &[0]).unwrap();
        assert_eq!(p.cells, [(0, 0, text("a")), (2, 0, text("b"))]);
        // …and a block taller than that selection doesn't fit.
        assert_eq!(
            plan(&copied(&[&["a"], &["b"], &["c"]]), &[0, 2], &[0]),
            Err(PasteRefusal::DoesNotFit {
                rows: 3,
                columns: 1
            })
        );
    }
    #[test]
    fn a_block_that_does_not_fit_is_refused_whole() {
        let p = plan(&copied(&[&["a", "b"]]), &[0], &[1]).unwrap_err();
        // Column 2 can't be edited: the whole paste is refused.
        assert_eq!(p, PasteRefusal::CellNotEditable);
        let (all_rows, all_columns) = grid(2, 2);
        let refuse = |rows: &[usize], columns: &[usize]| {
            plan_paste(
                &copied(&[&["a", "b", "c"], &["d", "e", "f"]]),
                &PasteTarget {
                    rows,
                    columns,
                    all_rows: &all_rows,
                    all_columns: &all_columns,
                },
                |_, _| true,
                |_| false,
                |_| false,
            )
        };
        // Three copied columns from the second of two columns: past the grid.
        assert_eq!(
            refuse(&[0], &[1]),
            Err(PasteRefusal::DoesNotFit {
                rows: 2,
                columns: 3
            })
        );
        // Wider than a selection of two columns.
        assert_eq!(
            refuse(&[0, 1], &[0, 1]),
            Err(PasteRefusal::DoesNotFit {
                rows: 2,
                columns: 3
            })
        );
        // Past the last row.
        let (r, c) = grid(1, 3);
        let p = plan_paste(
            &copied(&[&["a"], &["b"]]),
            &PasteTarget {
                rows: &[0],
                columns: &[0],
                all_rows: &r,
                all_columns: &c,
            },
            |_, _| true,
            |_| false,
            |_| false,
        );
        assert_eq!(
            p,
            Err(PasteRefusal::DoesNotFit {
                rows: 2,
                columns: 1
            })
        );
        assert!(
            PasteRefusal::DoesNotFit {
                rows: 2,
                columns: 3
            }
            .message()
            .contains("2 × 3")
        );
    }
    #[test]
    fn refusals_stage_nothing() {
        assert_eq!(
            plan(&copied(&[&["x"]]), &[3], &[0]),
            Err(PasteRefusal::DeletedRow)
        );
        assert_eq!(
            plan(&copied(&[&["x"]]), &[0], &[2]),
            Err(PasteRefusal::CellNotEditable)
        );
        assert_eq!(
            plan(&copied(&[&["a", "b"], &["c"]]), &[0], &[0]),
            Err(PasteRefusal::NotRectangular)
        );
        assert_eq!(
            plan(&copied(&[&["x"]]), &[], &[0]),
            Err(PasteRefusal::NothingSelected)
        );
        assert_eq!(plan(&[], &[0], &[0]), Err(PasteRefusal::NothingCopied));
        assert_eq!(
            PasteRefusal::DeletedRow.message(),
            "Deleted rows cannot be edited"
        );
        assert_eq!(
            PasteRefusal::CellNotEditable.message(),
            "A selected cell cannot be edited"
        );
    }
    #[test]
    fn whole_rows_map_back_to_their_columns() {
        let text = |v: &str| Cell::Text(v.into());
        // Shown as name, id (note hidden): values go back to their own columns.
        let rows = rows_by_layout(&copied(&[&["bob", "2"]]), &[1, 0], 3).unwrap();
        assert_eq!(rows, [vec![text("2"), text("bob"), Cell::unset()]]);
        // Every column of the table, in table order.
        let rows = rows_by_layout(&copied(&[&["1", "a", "n"]]), &[1, 0], 3).unwrap();
        assert_eq!(rows, [vec![text("1"), text("a"), text("n")]]);
        assert!(
            rows_by_layout(&copied(&[&["x"]]), &[1, 0], 3)
                .unwrap_err()
                .contains("1 values")
        );
    }
    #[test]
    fn copied_columns_are_found_by_name() {
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let table = names(&["id", "name", "email"]);
        assert_eq!(
            columns_by_name(&names(&["email", "id"]), &table, ""),
            Ok(vec![2, 0])
        );
        let missing = columns_by_name(&names(&["nick"]), &table, "zixma.users").unwrap_err();
        assert!(
            missing.contains("nick") && missing.contains("zixma.users"),
            "{missing}"
        );
        assert!(
            columns_by_name(&names(&["id"]), &names(&["id", "id"]), "")
                .unwrap_err()
                .contains("two columns")
        );
        assert!(
            columns_by_name(&names(&["id", "id"]), &table, "")
                .unwrap_err()
                .contains("two columns")
        );
        let text = |v: &str| Cell::Text(v.into());
        let rows = rows_by_names(
            &copied(&[&["b@x.io", "7", "bob"]]),
            &names(&["email", "id", "name"]),
            &table,
            "",
        )
        .unwrap();
        assert_eq!(rows, [vec![text("7"), text("bob"), text("b@x.io")]]);
        let rows = rows_by_names(&copied(&[&["bob"]]), &names(&["name"]), &table, "").unwrap();
        assert_eq!(rows, [vec![Cell::unset(), text("bob"), Cell::unset()]]);
    }
    #[test]
    fn null_and_default_keywords() {
        let p = plan(
            &copied(&[&["NULL", "DEFAULT"], &["DEFAULT", ""]]),
            &[0],
            &[0],
        )
        .unwrap();
        assert_eq!(
            p.cells,
            [
                (0, 0, Cell::Null),
                (0, 1, text("DEFAULT")),
                (1, 0, Cell::unset()),
                (1, 1, text(""))
            ]
        );
    }
    #[test]
    fn display_order_is_followed_not_data_order() {
        let p = plan_paste(
            &copied(&[&["a", "b"]]),
            &PasteTarget {
                rows: &[0],
                columns: &[2],
                all_rows: &[0],
                all_columns: &[2, 0, 1],
            },
            |_, _| true,
            |_| false,
            |_| false,
        )
        .unwrap();
        assert_eq!(p.cells, [(0, 2, text("a")), (0, 0, text("b"))]);
    }
}
