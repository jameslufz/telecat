//! Export to CSV / JSON / SQL INSERT. A table tab exports the whole table through its applied
//! filters and sort, streamed from the server; a query tab exports the rows it shows.
use super::*;
use std::io::Write;
use std::sync::atomic::AtomicU64;
use telecat_core::export::{ExportFormat, ExportWriter, insert_target};

/// A running table export: how far it got, and how to stop it.
pub(super) struct ExportJob {
    tab: u64,
    rows: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
}
/// What to export, decided before the save dialog opens.
enum Source {
    Table(TableDataRequest),
    Rows {
        columns: Vec<String>,
        binary: Vec<bool>,
        rows: Vec<Vec<Option<String>>>,
    },
}
/// `rows` (shown rows; `real[i]` is row i's index in the result) with each MySQL JSON cell
/// replaced by the server's JSON text, except cells `edited(row, column)` says were changed.
pub(super) fn with_json_text(
    mut rows: super::components::Rows,
    real: &[usize],
    json: &[(usize, Vec<Option<String>>)],
    edited: impl Fn(usize, usize) -> bool,
) -> super::components::Rows {
    for (shown, row) in rows.iter_mut().enumerate() {
        let Some(&r) = real.get(shown) else {
            continue;
        };
        for (col, text) in json {
            if let (Some(cell), Some(text)) = (row.get_mut(*col), text.get(r))
                && !edited(r, *col)
            {
                *cell = text.clone();
            }
        }
    }
    rows
}
/// The single table a query result's columns come from, when there is exactly one.
pub(super) fn single_source(tab: &Tab) -> Option<TableRef> {
    let plan = &tab.result_edit.as_ref()?.plan;
    let mut tables = plan.sources.tables.iter().map(|t| &t.table);
    let first = tables.next()?;
    tables.all(|t| t == first).then(|| first.clone())
}
/// Writes the export to a hidden file next to `path` and moves it into place only when it is
/// complete, so a failed or cancelled export never leaves a partial file under the chosen name.
fn partial_path(path: &std::path::Path) -> std::path::PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(".{name}.telecat-part"))
}
impl Workspace {
    pub(super) fn export_progress(&self) -> Option<SharedString> {
        let job = self.export_job.as_ref()?;
        Some(
            format!(
                "Exporting… {} rows. Click Export again to stop.",
                super::panels::grouped(job.rows.load(Ordering::Relaxed))
            )
            .into(),
        )
    }
    pub(super) fn export_result(&mut self, cx: &mut Context<Self>) {
        if let Some(job) = &self.export_job {
            if job.tab == self.state.active {
                job.cancel.store(true, Ordering::SeqCst);
            } else {
                self.show_hint("Another export is still running.", cx);
            }
            return;
        }
        let driver = self.state.connection.driver;
        let tab = self.state.current();
        let (name, target, source) = if let Some(t) = &tab.table {
            if t.is_new {
                self.show_hint(
                    "Save the table first (⌘S): there is nothing to export yet.",
                    cx,
                );
                return;
            }
            (
                t.table.name.clone(),
                insert_target(driver, &t.table),
                Source::Table(TableDataRequest {
                    table: t.table.clone(),
                    filters: t.applied.clone(),
                    combine: t.combine,
                    order_by: t.order_by.clone(),
                    estimate: false,
                    limit: 0,
                    offset: 0,
                }),
            )
        } else {
            let Some((columns, rows, real)) = self.visible_result(cx) else {
                return;
            };
            if columns.is_empty() {
                return;
            }
            // MySQL JSON exports as the server's JSON text, unless the cell was edited here.
            let rows = match telecat_app::workspace_model::export_json_text(
                &tab.grid.result,
                tab.result_edit.as_ref(),
            ) {
                Some(json) => with_json_text(rows, &real, json, |row, col| {
                    tab.result_edit.as_ref().is_some_and(|r| {
                        r.data.rows.get(row).is_none_or(|x| {
                            x.orig.is_none() || telecat_core::changes::cell_modified(x, col)
                        })
                    })
                }),
                None => rows,
            };
            let source = single_source(tab);
            let binary = tab
                .grid
                .result
                .binary
                .clone()
                .filter(|b| b.len() == columns.len())
                .unwrap_or_else(|| vec![false; columns.len()]);
            (
                source
                    .as_ref()
                    .map_or("query_result".into(), |t| t.name.clone()),
                source.as_ref().map_or_else(
                    || telecat_core::changes::quote_ident(driver, "query_result"),
                    |t| insert_target(driver, t),
                ),
                Source::Rows {
                    columns,
                    binary,
                    rows,
                },
            )
        };
        let session = self.session.clone();
        if matches!(source, Source::Table(_)) && session.is_none() {
            return;
        }
        let id = self.state.active;
        let directory = crate::export_dialog::default_directory();
        let runtime = self.services.runtime.clone();
        cx.spawn(async move |this, cx| {
            let path = match crate::export_dialog::choose_path(&directory, &name).await {
                Ok(Some(path)) => path,
                Ok(None) => return,
                Err(e) => {
                    let _ = this.update(cx, |this, cx| this.error(e, cx));
                    return;
                }
            };
            crate::export_dialog::remember_directory(&path);
            let format = ExportFormat::from_extension(
                path.extension().and_then(|s| s.to_str()).unwrap_or("csv"),
            );
            let partial = partial_path(&path);
            let result = match source {
                Source::Rows {
                    columns,
                    binary,
                    rows,
                } => {
                    let file = partial.clone();
                    cx.background_executor()
                        .spawn(async move {
                            let mut w = ExportWriter::new(format, driver, target, columns, binary);
                            let text = w.header() + &w.rows(&rows) + &w.footer();
                            std::fs::write(&file, text)?;
                            Ok::<_, anyhow::Error>(w.rows_written())
                        })
                        .await
                }
                Source::Table(req) => {
                    let rows = Arc::new(AtomicU64::new(0));
                    let cancel = Arc::new(AtomicBool::new(false));
                    let started = this
                        .update(cx, |this, cx| {
                            this.export_job = Some(ExportJob {
                                tab: id,
                                rows: rows.clone(),
                                cancel: cancel.clone(),
                            });
                            this.repaint_while_exporting(cx);
                            cx.notify();
                        })
                        .is_ok();
                    if !started {
                        return;
                    }
                    let session = session.expect("checked above");
                    let file = partial.clone();
                    let task = runtime.spawn(async move {
                        let mut out = std::io::BufWriter::new(std::fs::File::create(&file)?);
                        let mut writer: Option<ExportWriter> = None;
                        let mut failed: Option<std::io::Error> = None;
                        let n = session
                            .export_table(&req, &mut |columns, binary, chunk| {
                                if cancel.load(Ordering::SeqCst) {
                                    anyhow::bail!("The export was stopped.");
                                }
                                let w = match &mut writer {
                                    Some(w) => w,
                                    None => {
                                        let w = ExportWriter::new(
                                            format,
                                            driver,
                                            target.clone(),
                                            columns.to_vec(),
                                            binary.to_vec(),
                                        );
                                        out.write_all(w.header().as_bytes())?;
                                        writer.insert(w)
                                    }
                                };
                                if let Err(e) = out.write_all(w.rows(&chunk).as_bytes()) {
                                    failed = Some(e);
                                    anyhow::bail!("Writing the file failed");
                                }
                                rows.fetch_add(chunk.len() as u64, Ordering::Relaxed);
                                Ok(())
                            })
                            .await;
                        if let Some(e) = failed {
                            return Err(e.into());
                        }
                        let n = n?;
                        if let Some(w) = &writer {
                            out.write_all(w.footer().as_bytes())?;
                        }
                        out.flush()?;
                        Ok::<_, anyhow::Error>(n)
                    });
                    task.await.map_err(anyhow::Error::from).and_then(|r| r)
                }
            };
            let result = result.and_then(|n| {
                std::fs::rename(&partial, &path)?;
                Ok(n)
            });
            if result.is_err() {
                let _ = std::fs::remove_file(&partial);
            }
            let _ = this.update(cx, |this, cx| {
                if this.export_job.as_ref().is_some_and(|j| j.tab == id) {
                    this.export_job = None;
                }
                match result {
                    Ok(n) => {
                        this.show_hint(
                            format!(
                                "Exported {} rows to {}",
                                super::panels::grouped(n),
                                path.file_name().unwrap_or_default().to_string_lossy()
                            ),
                            cx,
                        );
                        cx.reveal_path(&path);
                    }
                    Err(e) if format!("{e:#}").contains("The export was stopped.") => {
                        this.show_hint("Export stopped: no file was written.", cx)
                    }
                    Err(e) if telecat_db::is_connection_lost(&e) => this.failed(e, cx),
                    Err(e) => this.error(format!("Export failed: {e:#}"), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }
    /// Repaints the progress while an export runs.
    fn repaint_while_exporting(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(250))
                    .await;
                let running = this
                    .update(cx, |this, cx| {
                        cx.notify();
                        this.export_job.is_some()
                    })
                    .unwrap_or(false);
                if !running {
                    break;
                }
            }
        })
        .detach();
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn query_exports_carry_the_servers_json_text_except_edited_cells() {
        let shown = vec![
            vec![
                Some("2".into()),
                Some("{\"n\":\"12345678901234567890\"}".into()),
            ],
            vec![Some("1".into()), Some("just a string".into())],
            vec![Some("3".into()), Some("typed by the user".into())],
        ];
        // The row filter hides result row 1; row 3 was edited here.
        let real = [2, 0, 3];
        let json = vec![(
            1,
            vec![
                Some("\"just a string\"".into()),
                Some("null".into()),
                Some("{\"n\": 12345678901234567890}".into()),
                Some("[1]".into()),
            ],
        )];
        let rows = super::with_json_text(shown, &real, &json, |row, _| row == 3);
        assert_eq!(rows[0][1].as_deref(), Some("{\"n\": 12345678901234567890}"));
        assert_eq!(rows[1][1].as_deref(), Some("\"just a string\""));
        assert_eq!(rows[2][1].as_deref(), Some("typed by the user"));
        assert_eq!(rows[0][0].as_deref(), Some("2"), "other columns untouched");
    }
    #[test]
    fn partial_files_are_hidden_next_to_the_target() {
        assert_eq!(
            super::partial_path(std::path::Path::new("/tmp/out/ธนาคาร.csv")),
            std::path::PathBuf::from("/tmp/out/.ธนาคาร.csv.telecat-part")
        );
    }
}
