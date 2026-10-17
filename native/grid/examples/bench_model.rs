#[allow(dead_code)]
#[path = "support/fixtures.rs"]
mod fixtures;
#[path = "support/perf.rs"]
mod perf;
use std::{hint::black_box, time::Instant};
use telecat_grid::{Rect, virtual_range};
fn main() {
    let before = perf::footprint();
    let start = Instant::now();
    let count = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(100000);
    let mut m = fixtures::large(count);
    let build = start.elapsed();
    m.viewport = Rect {
        x: 0.,
        y: 0.,
        width: 848.,
        height: 511.,
    };
    println!(
        "MODEL rows={} cols={} build_ms={:.3} storage_bytes={} phys_before={before:?} phys_after={:?}",
        m.rows.len(),
        m.columns.len(),
        build.as_secs_f64() * 1000.,
        m.storage_bytes(),
        perf::footprint()
    );
    let mut samples = Vec::with_capacity(10000);
    let mut max_cells = 0;
    for n in 0..10000 {
        let start = Instant::now();
        m.scroll_y = (n * 251 % 2600000) as f32;
        m.scroll_x = (n % 5 * 700) as f32;
        m.clamp_scroll();
        let l = m.layout();
        let range = virtual_range(m.rows.len(), m.scroll_y, l.height - 30., 20);
        let mut count = 0;
        for row in range {
            for col in m.visible_columns() {
                let r = m.cell_rect(telecat_grid::Cell { row, col });
                if r.x + r.width > m.number_width() && r.x < l.width {
                    black_box(m.cell_text(telecat_grid::Cell { row, col }));
                    count += 1;
                }
            }
        }
        max_cells = max_cells.max(count);
        samples.push(start.elapsed().as_secs_f64() * 1000.);
    }
    println!("MODEL max_overscanned_cells={max_cells}");
    perf::report("headless_window_lookup_NOT_frames", samples);
}
