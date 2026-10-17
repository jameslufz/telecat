//! One pending frame per window. The display link is subscribed only while this
//! latch is set; GPUI's invalidator/next-frame callbacks provide all new demand.
#[derive(Default)]
pub(crate) struct FrameDemand(bool);
impl FrameDemand {
    pub fn request(&mut self) {
        self.0 = true;
    }
    pub fn begin(&mut self) -> bool {
        std::mem::take(&mut self.0)
    }
    pub fn pending(&self) -> bool {
        self.0
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn idle_unsubscribes_and_stale_vsync_cannot_redraw() {
        let mut demand = FrameDemand::default();
        demand.request();
        demand.request(); // notifications coalesce
        assert!(demand.begin());
        assert!(!demand.pending()); // unsubscribe after painting
        for _ in 0..600 {
            assert!(!demand.begin());
        }
        demand.request(); // input after an arbitrarily long idle period
        assert!(demand.begin());
        assert!(!demand.pending());
    }
    #[test]
    fn invalidation_during_draw_and_throttled_retry_keep_vsync() {
        let mut demand = FrameDemand::default();
        demand.request();
        for _ in 0..120 {
            assert!(demand.begin());
            demand.request(); // animation or GPUI's throttled-frame retry
            assert!(demand.pending());
        }
        assert!(demand.begin());
        assert!(!demand.pending()); // final animation frame
    }
}
