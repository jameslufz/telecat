pub fn footprint() -> Option<u64> {
    #[cfg(target_os = "macos")]
    unsafe {
        let mut usage: libc::rusage_info_v2 = std::mem::zeroed();
        let code = libc::proc_pid_rusage(
            std::process::id() as i32,
            libc::RUSAGE_INFO_V2,
            (&mut usage as *mut libc::rusage_info_v2).cast(),
        );
        if code == 0 {
            Some(usage.ri_phys_footprint)
        } else {
            None
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}
pub fn report(label: &str, mut values: Vec<f64>) {
    if values.is_empty() {
        println!("PERF {label} unavailable");
        return;
    }
    values.sort_by(f64::total_cmp);
    let n = values.len();
    let sum: f64 = values.iter().sum();
    println!(
        "PERF {label} count={n} mean_ms={:.4} p50_ms={:.4} p95_ms={:.4} p99_ms={:.4} max_ms={:.4}",
        sum / n as f64,
        values[n / 2],
        values[(n * 95 / 100).min(n - 1)],
        values[(n * 99 / 100).min(n - 1)],
        values[n - 1]
    );
}
