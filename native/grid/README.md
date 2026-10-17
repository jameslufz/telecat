# telecat-grid

Standalone GPUI data grid for Data tabs and query results. All database operations belong to the caller. Initialize GPUI Kit, create `cx.new(|cx| DataGrid::new(model, cx))`, and render the entity in a bounded flex child. The component installs its own Tab bindings once per App.

`GridModel::new(columns, rows)` accepts `Vec<Vec<Option<String>>>` and packs its text into `RowStore`: one UTF-8 arena per 1000-row page, two `u32` offsets per cell, and a separate NULL sentinel. `GridModel::from_rows` accepts an iterator, avoiding a full temporary row vector for large synthetic or streamed results. It samples at most 60 rows when computing widths. Read through `model.rows.get(row)`, `Row::get(column)` or `model.cell_value(cell)`; use `rows.set` / `replace_row` when accepting caller edits. These operations preserve NULL, empty strings, Thai and emoji, and compact stale text after repeated edits. `Column` carries name, numeric alignment, editability, visibility and popup-chevron metadata. `Options` carries row offset, empty-state text, row-number visibility, EMPTY display and enabled hooks. Query results normally enable only detail and Quick Look.

Subscribe to `GridEvent` with `cx.subscribe` or `cx.subscribe_in`. Selection and resized widths update locally and emit snapshots. Edit and sort events are requests: **the grid never writes row data or sorts the result itself**. Accept an event using `grid.update(cx, |grid, cx| grid.update_model(|model| { /* apply caller state */ }, cx))`. The closure mutates the owned model without copying the result. Rejected edits/sorts leave the supplied data/indicator unchanged. Use `GridModel::replace_data` to replace results while preserving resized widths for the same NUL-joined column names, like Electron.

`Edits` contains sparse row states, modified cells and DEFAULT cells. `None` means SQL NULL. Put unset insert cells in `edits.defaults` as well, so they render/copy as DEFAULT. Missing trailing values also render DEFAULT. `RowState::Modified` does not tint a whole row: Electron colors individual modified cells with `MOD_CELL`; `MOD` is exported for the surrounding dirty-state UI. New/deleted row colors override selection; selected-cell outlines remain visible.

Editing emits the exact string. Leaving a NULL/DEFAULT cell empty emits no change. Literal `NULL` is a string; setting SQL NULL/DEFAULT remains a caller menu action. Call `request_edit(cell, nonce, window, cx)` for the + Row workflow. Editing uses the toolkit's unstyled text engine for native IME/Unicode/undo, inside the custom cell frame. Alt+Enter inserts a newline and expands the editor. Enter commits, Escape cancels, Tab/Shift+Tab commit and move to the next/previous visible editable column in the same row. Blur commits. Before taking a commit or navigation snapshot, call `DataGrid::flush_edit(cx)` and apply the returned events synchronously; this reads the active input engine even before deferred blur/change callbacks arrive. The multiline resize grip changes its size.

ContextMenu and CellPopup events carry a cell and its **window-local point bounds**, not screen coordinates. The caller displays its native context menu, prepends extra menu items, then appends `COPY_MENU`. Route the chosen ID through `activate_menu`; standard copy IDs write the clipboard and unknown IDs emit `GridEvent::Menu`. Middle-click emits QuickLook, Option-click opens the context hook, and Space emits ToggleDetail. Enable insert/duplicate/delete/paste hooks for table-data actions. Set `hooks.escape` while the parent would consume Escape (e.g. dirty changes); the Escape event preserves selection in that case.

The component handles keyboard clipboard shortcuts and the toolkit's native Edit-menu Copy/Paste actions. It also exports `Copy`/`Paste` actions and direct `copy_selection`/`paste_clipboard` methods for shell menu routing. Copy uses all columns, including hidden ones. With one selected row and an active column it copies one cell; otherwise it copies selected rows in ascending order. TSV is deliberately unquoted. Paste removes all CR characters and exactly one final LF, then splits on LF and tab; it does not interpret NULL or quotation marks.

Electron's arrows (including their anchor and hidden-column behavior) are retained. Requested PageUp/PageDown/Home/End and non-editing Tab navigation are extensions because DataGrid.tsx has no explicit handlers for those keys. Home/End select the first/last result row; page keys move by the body viewport's full-row count; Tab visits visible columns and wraps rows, clamping at result boundaries.

Rendering uses one canvas and clips both axes. Only intersecting columns and viewport rows are shaped/painted; virtual bounds expose Electron's 20-row overscan. No element or shaped-text cache grows with the result's row count. A single editor exists while editing. The grid has no repeating idle timer; long-value/header tooltips use a cancellable, one-shot delay. `FrameStats` retains at most 1,200 CPU preparation/paint samples, separately from actual presentation timing.

## Verification and captures

From `native/`, always source the shared environment first:

```sh
. scripts/env.sh
node grid/tools/oracle.mjs
cargo test -p telecat-grid
cargo build --release -p telecat-grid --examples
./grid/tools/scenario.sh grid-07
```

The launcher creates a temporary profile under `grid/`, never reads the real Keychain, and makes no database connections. The example binaries have grid-specific names to avoid collisions with the editor session in the shared target directory. It opens a rectangular component-only window and prints `WINDOW <name> <CGWindowID>`. Use one scenario at a time. Screenshots are left to the reviewer.

| Scenario | Logical size | Reference crop in 2x pixels (x, y, width, height) |
| --- | --- | --- |
| grid-07 | 848 × 511 | 07-table-data.png: 542, 184, 1696, 1022 |
| grid-51 | 848 × 511 | 51-data-staged.png: 542, 184, 1696, 1022 |
| grid-06 | 848 × 233.5 | 06-query-all-rowselected.png: 542, 739, 1696, 467 |
| grid-22 | 848 × 511 | 22-pg-table.png: 542, 184, 1696, 1022 |
| grid-100k | 848 × 511 | 100,000 × 30 performance fixture |
| grid-empty | 848 × 511 | Additional empty-message/filler fixture |

`grid-51` starts at horizontal offset 526.5 pt and selects row 7 / country_code. Its modified-cell state is offscreen, matching the reference's visible colors; scroll left to see it. The visible bank widths are measured from the reference. The final, clipped swift_code width is calibrated to 131 pt from the reference scrollbar extent rather than recomputed from the unknown offscreen data.

```sh
TELECAT_GRID_PERF=1 ./grid/tools/scenario.sh grid-100k
./target/release/examples/grid-bench-model
```

The GUI perf mode sweeps for 360 frames, printing frame-interval percentiles, grid CPU paint percentiles, process phys_footprint and maximum painted-cell count, then stops scheduling frames. Frame intervals include platform/display scheduling, not just grid work. The headless benchmark reports owned model bytes, process footprint, creation time and bounded window-lookup times; **those lookup times are not rendering/frame timings**.

The tests include 2,107 deterministic cases produced by AST extraction and execution of the actual Electron functions (source SHA-256 in `tests/oracle.json`), additional boundary/extension tests, and GPUI dispatch tests using the same Base Root as the scenario. Headless rendering validates layout and interactions, not font rasterization or screenshot equality.

Integration memory measurements and shared-font validation are recorded in `../reports/M3-M4-integration.txt`. Theme tokens and CSS font-family resolution now come from `telecat-theme`; `DataGrid::new` initializes it, and hosts can also call `telecat_theme::init(cx)`.
