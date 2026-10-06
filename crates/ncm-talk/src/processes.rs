//! Read-only native process metadata; never launches process-table utilities.
use crate::discovery::ProcessInfo;

#[cfg(target_os = "macos")]
fn info(pid: u32) -> Option<libc::proc_bsdinfo> {
    let mut data = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
    // SAFETY: aligned C-layout buffer of the exact size; only read after a full result.
    let count = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            data.as_mut_ptr().cast(),
            size,
        )
    };
    if count != size {
        return None;
    }
    let data = unsafe { data.assume_init() };
    (data.pbi_status != libc::SZOMB).then_some(data)
}

#[cfg(target_os = "macos")]
pub(crate) fn identity(pid: u32) -> Option<u64> {
    let data = info(pid)?;
    Some(data.pbi_start_tvsec * 1_000_000 + data.pbi_start_tvusec)
}

#[cfg(target_os = "macos")]
pub(crate) fn snapshot() -> std::io::Result<Vec<ProcessInfo>> {
    // SAFETY: the null query only reports capacity; the second query receives an
    // aligned pid buffer. Retry if processes appeared while allocating the buffer.
    let needed = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if needed <= 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut pids = vec![0i32; needed as usize + 64];
    let count = loop {
        let count = unsafe {
            libc::proc_listallpids(
                pids.as_mut_ptr().cast(),
                std::mem::size_of_val(pids.as_slice()) as i32,
            )
        };
        if count <= 0 {
            return Err(std::io::Error::last_os_error());
        }
        if (count as usize) < pids.len() {
            break count as usize;
        }
        pids.resize(pids.len() * 2, 0);
    };
    Ok(pids[..count]
        .iter()
        .filter_map(|&pid| {
            let data = info(pid as u32)?;
            let name = if data.pbi_name[0] != 0 {
                &data.pbi_name[..]
            } else {
                &data.pbi_comm[..]
            };
            let bytes: Vec<_> = name
                .iter()
                .take_while(|&&b| b != 0)
                .map(|&b| b as u8)
                .collect();
            let mut command = String::from_utf8_lossy(&bytes).into_owned();
            // pbi_name may contain a display label while pbi_comm retains the executable.
            let comm_bytes: Vec<_> = data
                .pbi_comm
                .iter()
                .take_while(|&&b| b != 0)
                .map(|&b| b as u8)
                .collect();
            let comm = String::from_utf8_lossy(&comm_bytes);
            command = command_label(&command, &comm).to_owned();
            // Native Claude installations execute a version-named binary. Both BSD labels
            // can be "2.1.289", while ps's argv-derived label still says "claude".
            if command.bytes().next().is_some_and(|b| b.is_ascii_digit()) {
                let mut path = [0u8; 4096];
                let count =
                    unsafe { libc::proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32) };
                if count > 0 {
                    let path = String::from_utf8_lossy(&path[..count as usize])
                        .trim_end_matches('\0')
                        .to_owned();
                    if crate::harness_for_command(&path).is_some() {
                        command = path;
                    }
                }
            }
            Some(ProcessInfo {
                pid: data.pbi_pid,
                ppid: data.pbi_ppid,
                command,
            })
        })
        .collect())
}

#[cfg(any(target_os = "macos", test))]
fn command_label<'a>(name: &'a str, comm: &'a str) -> &'a str {
    if crate::harness_for_command(name).is_none() && crate::harness_for_command(comm).is_some() {
        comm
    } else {
        name
    }
}

/// Live epoch start time and exact local/UTC lstart labels. Claude can write procStart
/// in UTC even when the viewer inherits the machine's local timezone.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn start(pid: u32) -> Option<(ncm_model::Timestamp, [String; 2])> {
    #[cfg(target_os = "macos")]
    let millis = {
        let data = info(pid)?;
        (data.pbi_start_tvsec * 1000 + data.pbi_start_tvusec / 1000) as i64
    };
    #[cfg(target_os = "linux")]
    let millis = {
        let ticks = identity(pid)?;
        let boot = std::fs::read_to_string("/proc/stat")
            .ok()?
            .lines()
            .find_map(|line| line.strip_prefix("btime ")?.parse::<i64>().ok())?;
        let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        if hz <= 0 {
            return None;
        }
        boot * 1000 + (ticks * 1000 / hz as u64) as i64
    };
    let seconds = (millis / 1000) as libc::time_t;
    let mut labels = [String::new(), String::new()];
    for (i, label) in labels.iter_mut().enumerate() {
        let mut clock = std::mem::MaybeUninit::<libc::tm>::zeroed();
        let mut bytes = [0u8; 64];
        // SAFETY: both time converters initialize tm; strftime receives its full buffer size.
        let size = unsafe {
            let clock_ptr = if i == 0 {
                libc::localtime_r(&seconds, clock.as_mut_ptr())
            } else {
                libc::gmtime_r(&seconds, clock.as_mut_ptr())
            };
            if clock_ptr.is_null() {
                return None;
            }
            libc::strftime(
                bytes.as_mut_ptr().cast(),
                bytes.len(),
                c"%a %b %e %T %Y".as_ptr(),
                clock.as_ptr(),
            )
        };
        *label = String::from_utf8_lossy(&bytes[..size]).into_owned();
    }
    Some((ncm_model::Timestamp(millis), labels))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn start(_pid: u32) -> Option<(ncm_model::Timestamp, [String; 2])> {
    None
}

#[cfg(target_os = "linux")]
fn info(pid: u32) -> Option<(ProcessInfo, u64)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (head, tail) = stat.rsplit_once(')')?;
    let fields: Vec<_> = tail.split_whitespace().collect();
    if fields.first() == Some(&"Z") {
        return None;
    }
    Some((
        ProcessInfo {
            pid,
            ppid: fields.get(1)?.parse().ok()?,
            command: head.split_once('(')?.1.into(),
        },
        fields.get(19)?.parse().ok()?,
    ))
}

#[cfg(target_os = "linux")]
pub(crate) fn identity(pid: u32) -> Option<u64> {
    Some(info(pid)?.1)
}

#[cfg(target_os = "linux")]
pub(crate) fn snapshot() -> std::io::Result<Vec<ProcessInfo>> {
    Ok(std::fs::read_dir("/proc")?
        .filter_map(|entry| {
            let pid = entry.ok()?.file_name().to_str()?.parse().ok()?;
            Some(info(pid)?.0)
        })
        .collect())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn identity(_pid: u32) -> Option<u64> {
    None
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn snapshot() -> std::io::Result<Vec<ProcessInfo>> {
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn executable_comm_wins_over_unrecognized_display_name() {
        assert_eq!(command_label("2.1.289", "claude"), "claude");
        assert_eq!(command_label("display label", "codex"), "codex");
        assert_eq!(command_label("codex", "node"), "codex");
        assert_eq!(command_label("2.1.289", "2.1.289"), "2.1.289");
    }
    #[test]
    fn native_snapshot_contains_self_and_stable_start_identity() {
        let pid = std::process::id();
        let identity_before = identity(pid).unwrap();
        let snapshot = snapshot().unwrap();
        assert!(snapshot
            .iter()
            .any(|p| p.pid == pid && p.ppid != 0 && !p.command.is_empty()));
        assert_eq!(identity(pid), Some(identity_before));
    }
}
