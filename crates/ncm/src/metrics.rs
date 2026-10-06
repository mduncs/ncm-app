use anyhow::Result;
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::Path,
    time::Instant,
};
pub struct Metrics {
    start: Instant,
    sample: Instant,
    cpu: f64,
    children: f64,
    pub image_bytes: usize,
    pub captures: usize,
    pub changed: usize,
    pub writes: usize,
    pub segment: String,
    pub show: bool,
    log: Option<File>,
}
impl Metrics {
    pub fn new(show: bool, path: Option<&Path>) -> Result<Self> {
        let now = Instant::now();
        let mut metrics = Self {
            start: now,
            sample: now,
            cpu: cpu_seconds(libc::RUSAGE_SELF),
            children: children_cpu_seconds(),
            image_bytes: 0,
            captures: 0,
            changed: 0,
            writes: 0,
            segment: String::new(),
            show,
            log: path
                .map(|p| OpenOptions::new().create(true).append(true).open(p))
                .transpose()?,
        };
        metrics.trace("start", &format!("pid={}", std::process::id()));
        Ok(metrics)
    }
    pub fn trace(&mut self, event: &str, detail: &str) {
        if let Some(log) = &mut self.log {
            let line = format!(
                "t_ms={:.3} mono_ns={} event={event} {detail}\n",
                self.start.elapsed().as_secs_f64() * 1000.0,
                monotonic_ns()
            );
            let _ = log.write_all(line.as_bytes());
        }
    }
    pub fn enabled(&self) -> bool {
        self.show || self.log.is_some()
    }
    pub fn sample(&mut self) {
        if !self.enabled() {
            return;
        }
        let elapsed = self.sample.elapsed().as_secs_f64();
        let cpu = cpu_seconds(libc::RUSAGE_SELF);
        let children = children_cpu_seconds();
        let percent = (cpu - self.cpu) / elapsed * 100.0;
        let child_percent = (children - self.children).max(0.0) / elapsed * 100.0;
        let rss = resident_bytes() as f64 / (1024.0 * 1024.0);
        let kb = self.image_bytes as f64 / 1000.0 / elapsed;
        let captures = self.captures as f64 / elapsed;
        self.segment =
            format!("CPU {percent:.1}% · {rss:.1}MB · {kb:.0}KB/s · {captures:.1} cap/s");
        self.trace("sample", &format!("interval_s={elapsed:.3} cpu_pct={percent:.3} child_cpu_pct={child_percent:.3} rss_mb={rss:.3} peak_rss_mb={:.3} image_kbs={kb:.3} captures_s={captures:.3} changed={} writes={}", peak_resident_bytes() as f64 / (1024.0 * 1024.0), self.changed, self.writes));
        self.sample = Instant::now();
        self.cpu = cpu;
        self.children = children;
        self.image_bytes = 0;
        self.captures = 0;
        self.changed = 0;
        self.writes = 0;
    }
}
fn cpu_seconds(who: libc::c_int) -> f64 {
    // SAFETY: getrusage initializes the supplied structure.
    unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        if libc::getrusage(who, &mut usage) != 0 {
            return 0.0;
        }
        (usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) as f64
            + (usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) as f64 / 1_000_000.0
    }
}
// getrusage(CHILDREN) excludes a long-lived control client until it exits.
// Include running direct children without spawning ps; once reaped their time
// naturally moves into getrusage. Metrics run after query/capture work completes.
fn children_cpu_seconds() -> f64 {
    cpu_seconds(libc::RUSAGE_CHILDREN) + running_children_cpu_seconds()
}
#[cfg(target_os = "macos")]
fn running_children_cpu_seconds() -> f64 {
    unsafe {
        let mut pids = [0 as libc::pid_t; 128];
        let count = libc::proc_listchildpids(
            libc::getpid(),
            pids.as_mut_ptr().cast(),
            std::mem::size_of_val(&pids) as i32,
        );
        let mut total = 0_u64;
        for &pid in pids.iter().take(count.max(0) as usize) {
            let mut info: libc::proc_taskinfo = std::mem::zeroed();
            let size = std::mem::size_of_val(&info) as i32;
            if pid > 0
                && libc::proc_pidinfo(
                    pid,
                    libc::PROC_PIDTASKINFO,
                    0,
                    (&mut info as *mut libc::proc_taskinfo).cast(),
                    size,
                ) == size
            {
                total += info.pti_total_user + info.pti_total_system;
            }
        }
        total as f64 / 1_000_000_000.0
    }
}
#[cfg(not(target_os = "macos"))]
fn running_children_cpu_seconds() -> f64 {
    let children: std::collections::HashSet<_> = std::fs::read_dir("/proc/self/task")
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter_map(|task| std::fs::read_to_string(task.path().join("children")).ok())
        .flat_map(|pids| {
            pids.split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect();
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as f64;
    children
        .iter()
        .filter_map(|pid| {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            let fields: Vec<_> = stat.rsplit_once(')')?.1.split_whitespace().collect();
            Some(
                (fields.get(11)?.parse::<u64>().ok()? + fields.get(12)?.parse::<u64>().ok()?)
                    as f64
                    / ticks,
            )
        })
        .sum()
}

#[cfg(target_os = "macos")]
fn resident_bytes() -> u64 {
    // SAFETY: valid taskinfo buffer, its exact size, and our own pid.
    unsafe {
        let mut info: libc::proc_taskinfo = std::mem::zeroed();
        let size = std::mem::size_of_val(&info) as i32;
        if libc::proc_pidinfo(
            libc::getpid(),
            libc::PROC_PIDTASKINFO,
            0,
            (&mut info as *mut libc::proc_taskinfo).cast(),
            size,
        ) == size
        {
            info.pti_resident_size
        } else {
            0
        }
    }
}
#[cfg(not(target_os = "macos"))]
fn resident_bytes() -> u64 {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok())
        .unwrap_or(0)
        * 4096
}

fn peak_resident_bytes() -> u64 {
    // SAFETY: getrusage initializes the supplied structure for our own process.
    unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, &mut usage) != 0 {
            return 0;
        }
        let bytes = usage.ru_maxrss.max(0) as u64;
        if cfg!(target_os = "macos") {
            bytes
        } else {
            bytes * 1024
        }
    }
}

fn monotonic_ns() -> u64 {
    // SAFETY: valid initialized timespec for the system monotonic clock.
    unsafe {
        let mut ts: libc::timespec = std::mem::zeroed();
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
        ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn stats_include_a_running_child_before_it_is_reaped() {
        let before = super::running_children_cpu_seconds();
        let mut child = std::process::Command::new("sh")
            .args([
                "-c",
                "i=0; while [ $i -lt 50000 ]; do i=$((i + 1)); done; exec sleep 10",
            ])
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let observed = loop {
            let cpu = super::running_children_cpu_seconds();
            if cpu > before + 0.001 || std::time::Instant::now() >= deadline {
                break cpu;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        let _ = child.kill();
        let _ = child.wait();
        assert!(observed > before + 0.001, "running child CPU was omitted");
    }
}
