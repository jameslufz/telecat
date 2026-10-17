//! Optional one-shot launch timing, including dyld/process startup when macOS exposes it.
use std::time::{Duration, Instant};
pub struct LaunchTiming {
    main_entry: Instant,
    process_start: Option<(u64, f64)>,
}
impl LaunchTiming {
    pub fn from_env() -> Option<Self> {
        (std::env::var_os("TELECAT_LAUNCH_TIMING").as_deref() == Some(std::ffi::OsStr::new("1")))
            .then(Self::start)
    }
    pub fn start() -> Self {
        let main_entry = Instant::now();
        let process_start = unsafe {
            let mut usage: libc::rusage_info_v0 = std::mem::zeroed();
            let mut timebase: mach2::mach_time::mach_timebase_info = std::mem::zeroed();
            let ok = libc::proc_pid_rusage(
                libc::getpid(),
                libc::RUSAGE_INFO_V0,
                (&mut usage as *mut libc::rusage_info_v0).cast(),
            ) == 0
                && mach2::mach_time::mach_timebase_info(&mut timebase) == 0
                && timebase.denom != 0
                && usage.ri_proc_start_abstime != 0;
            ok.then_some((
                usage.ri_proc_start_abstime,
                timebase.numer as f64 / timebase.denom.max(1) as f64,
            ))
        };
        Self {
            main_entry,
            process_start,
        }
    }
    pub fn elapsed(&self) -> Duration {
        if let Some((start, nanos_per_tick)) = self.process_start {
            let ticks = unsafe { mach2::mach_time::mach_absolute_time() }.saturating_sub(start);
            Duration::from_secs_f64(ticks as f64 * nanos_per_tick / 1e9)
        } else {
            self.main_entry.elapsed()
        }
    }
    pub fn report(self) {
        eprintln!(
            "LAUNCH welcome_first_frame_ms={:.3} clock={} measurement=after-first-presentation",
            self.elapsed().as_secs_f64() * 1000.,
            if self.process_start.is_some() {
                "process-start"
            } else {
                "main-entry-fallback"
            }
        );
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn launch_clock_is_positive_and_monotonic() {
        let timing = super::LaunchTiming::start();
        let before = timing.elapsed();
        assert!(!before.is_zero());
        assert!(timing.elapsed() >= before);
    }
}
