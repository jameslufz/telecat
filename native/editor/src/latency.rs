//! CPU keystroke-to-paint instrumentation. Native runs include CoreText and GPUI
//! painting; test-platform runs do not establish display-presentation latency.
use std::{cell::RefCell, rc::Rc, time::Instant};
#[derive(Clone, Default)]
pub struct LatencyProbe(Rc<RefCell<Measurements>>);
#[derive(Default)]
struct Measurements {
    start: Option<Instant>,
    armed: bool,
    samples: Vec<f64>,
}
#[derive(Clone, Copy, Debug)]
pub struct LatencySummary {
    pub count: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub max_ms: f64,
}
impl LatencyProbe {
    pub fn begin(&self) {
        let mut m = self.0.borrow_mut();
        m.start = Some(Instant::now());
        m.armed = false;
    }
    pub fn arm(&self) {
        self.0.borrow_mut().armed = true;
    }
    pub fn painted(&self) {
        let mut m = self.0.borrow_mut();
        if !m.armed {
            return;
        }
        if let Some(start) = m.start.take() {
            m.samples.push(start.elapsed().as_secs_f64() * 1000.);
        }
    }
    pub fn summary(&self) -> LatencySummary {
        let mut values = self.0.borrow().samples.clone();
        values.sort_by(f64::total_cmp);
        let n = values.len();
        let at = |p: f64| {
            if n == 0 {
                0.
            } else {
                values[((n as f64 * p).ceil() as usize)
                    .saturating_sub(1)
                    .min(n - 1)]
            }
        };
        LatencySummary {
            count: n,
            p50_ms: at(0.5),
            p95_ms: at(0.95),
            max_ms: at(1.),
        }
    }
    pub fn clear(&self) {
        let mut m = self.0.borrow_mut();
        m.start = None;
        m.samples.clear();
    }
}
