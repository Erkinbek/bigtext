//! Process memory statistics (for the status/debug overlay and benchmarks) and free disk space.

use std::path::Path;

/// Bytes this user may still write on the drive holding `dir` (or its nearest existing
/// parent). `BIGTEXT_FAKE_FREE` overrides it, for tests. Slow on a dead network share: call it
/// from a worker.
pub fn free_space(dir: &Path) -> Option<u64> {
    if let Some(v) = std::env::var_os("BIGTEXT_FAKE_FREE") {
        return v.to_str()?.parse().ok();
    }
    let dir = dir.ancestors().find(|d| d.is_dir())?;
    space::free(dir)
}

#[cfg(windows)]
mod space {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetDiskFreeSpaceExW(dir: *const u16, available: *mut u64, total: *mut u64, free: *mut u64) -> i32;
    }

    pub fn free(dir: &std::path::Path) -> Option<u64> {
        let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut available = 0u64;
        // SAFETY: `wide` is NUL-terminated; the totals it does not need may be null.
        let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut available, std::ptr::null_mut(), std::ptr::null_mut()) };
        (ok != 0).then_some(available)
    }
}

#[cfg(unix)]
mod space {
    pub fn free(dir: &std::path::Path) -> Option<u64> {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: `c` is a NUL-terminated path and `st` a statvfs to fill.
        let ok = unsafe { libc::statvfs(c.as_ptr(), &mut st) } == 0;
        #[allow(clippy::unnecessary_cast)]
        ok.then(|| st.f_bavail as u64 * st.f_frsize as u64)
    }
}

#[cfg(not(any(windows, unix)))]
mod space {
    pub fn free(_dir: &std::path::Path) -> Option<u64> {
        None
    }
}

/// Private (non-file-backed) committed memory of this process in bytes, if available.
pub fn private_bytes() -> Option<u64> {
    imp::private_bytes()
}

/// Peak private memory, if available.
pub fn peak_private_bytes() -> Option<u64> {
    imp::peak_private_bytes()
}

#[cfg(windows)]
mod imp {
    #[repr(C)]
    #[allow(non_snake_case)]
    struct ProcessMemoryCountersEx {
        cb: u32,
        PageFaultCount: u32,
        PeakWorkingSetSize: usize,
        WorkingSetSize: usize,
        QuotaPeakPagedPoolUsage: usize,
        QuotaPagedPoolUsage: usize,
        QuotaPeakNonPagedPoolUsage: usize,
        QuotaNonPagedPoolUsage: usize,
        PagefileUsage: usize,
        PeakPagefileUsage: usize,
        PrivateUsage: usize,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> isize;
        fn K32GetProcessMemoryInfo(process: isize, counters: *mut ProcessMemoryCountersEx, cb: u32) -> i32;
    }

    fn counters() -> Option<ProcessMemoryCountersEx> {
        let mut c: ProcessMemoryCountersEx = unsafe { std::mem::zeroed() };
        c.cb = std::mem::size_of::<ProcessMemoryCountersEx>() as u32;
        // SAFETY: valid pointer and size; GetCurrentProcess returns a pseudo-handle.
        let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) };
        (ok != 0).then_some(c)
    }

    pub fn private_bytes() -> Option<u64> {
        counters().map(|c| c.PrivateUsage as u64)
    }

    pub fn peak_private_bytes() -> Option<u64> {
        counters().map(|c| c.PeakPagefileUsage as u64)
    }
}

#[cfg(target_os = "linux")]
mod imp {
    fn status_kb(key: &str) -> Option<u64> {
        let s = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = s.lines().find(|l| l.starts_with(key))?;
        line.split_whitespace().nth(1)?.parse::<u64>().ok().map(|kb| kb * 1024)
    }
    pub fn private_bytes() -> Option<u64> {
        status_kb("RssAnon:")
    }
    pub fn peak_private_bytes() -> Option<u64> {
        status_kb("VmHWM:")
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod imp {
    pub fn private_bytes() -> Option<u64> {
        None
    }
    pub fn peak_private_bytes() -> Option<u64> {
        None
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn free_space_of_the_temp_drive() {
        let free = super::free_space(&std::env::temp_dir().join("no").join("such").join("dir"));
        assert!(free.is_some_and(|f| f > 0), "{free:?}");
    }
}
