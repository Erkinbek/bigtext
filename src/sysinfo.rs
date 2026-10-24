//! Process memory statistics (for the status/debug overlay and benchmarks).

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
