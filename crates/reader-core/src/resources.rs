use crate::Result;
use serde::Serialize;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Mutex, MutexGuard, PoisonError, TryLockError,
};

const MIB: u64 = 1024 * 1024;
const READER_RESERVE_MIB: u64 = 1024;

pub(crate) static RESOURCES: ResourceGovernor = ResourceGovernor::new();

#[derive(Clone, Copy)]
pub(crate) enum InferencePriority {
    Foreground,
    Background,
}

pub(crate) struct ResourceGovernor {
    work: Mutex<()>,
    interactive_waiting: AtomicUsize,
}

pub(crate) struct OptionalJob<'a> {
    governor: &'a ResourceGovernor,
    _work: MutexGuard<'a, ()>,
}

impl OptionalJob<'_> {
    pub(crate) fn should_yield(&self) -> bool {
        self.governor.interactive_waiting.load(Ordering::SeqCst) != 0
    }
}

impl ResourceGovernor {
    const fn new() -> Self {
        Self {
            work: Mutex::new(()),
            interactive_waiting: AtomicUsize::new(0),
        }
    }

    pub(crate) fn interactive(&self) -> MutexGuard<'_, ()> {
        self.interactive_waiting.fetch_add(1, Ordering::SeqCst);
        // The mutex protects a permit rather than mutable model state.
        let guard = self.work.lock().unwrap_or_else(PoisonError::into_inner);
        self.interactive_waiting.fetch_sub(1, Ordering::SeqCst);
        guard
    }

    /// A manual model request waits for the running job to yield, but still
    /// yields to subsequent foreground work at its inference checkpoints.
    pub(crate) fn requested_model(&self) -> OptionalJob<'_> {
        OptionalJob {
            governor: self,
            _work: self.interactive(),
        }
    }

    pub(crate) fn requested_model_until(&self, canceled: &dyn Fn() -> bool) -> Option<OptionalJob<'_>> {
        self.interactive_until(canceled).map(|_work| OptionalJob { governor: self, _work })
    }

    pub(crate) fn interactive_until(&self, canceled: &dyn Fn() -> bool) -> Option<MutexGuard<'_, ()>> {
        self.interactive_waiting.fetch_add(1, Ordering::SeqCst);
        let work = loop {
            if canceled() {
                break None;
            }
            match self.work.try_lock() {
                Ok(work) => break Some(work),
                Err(TryLockError::Poisoned(error)) => break Some(error.into_inner()),
                Err(TryLockError::WouldBlock) => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            }
        };
        self.interactive_waiting.fetch_sub(1, Ordering::SeqCst);
        work
    }

    pub(crate) fn optional(&self) -> Result<OptionalJob<'_>> {
        let busy = "Читай обробляє інший запит. Спробуйте ще раз.";
        if self.interactive_waiting.load(Ordering::SeqCst) != 0 {
            return Err(busy.into());
        }
        let work = match self.work.try_lock() {
            Ok(work) => work,
            Err(TryLockError::WouldBlock) => return Err(busy.into()),
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
        };
        let job = OptionalJob {
            governor: self,
            _work: work,
        };
        if job.should_yield() {
            return Err(busy.into());
        }
        Ok(job)
    }
}

#[derive(Clone, Copy)]
pub(crate) struct ModelBudget {
    pub working_mib: u64,
    pub minimum_total_mib: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryReadiness {
    pub ready: bool,
    pub available_mib: Option<u64>,
    pub required_mib: u64,
    pub total_mib: Option<u64>,
    pub minimum_total_mib: u64,
}

impl ModelBudget {
    pub(crate) fn readiness(self) -> MemoryReadiness {
        let memory = system_memory().ok();
        self.assess(memory)
    }

    fn assess(self, memory: Option<(u64, u64)>) -> MemoryReadiness {
        let total_mib = memory.map(|(total, _)| total / MIB);
        let available_mib = memory.map(|(total, available)| available.min(total) / MIB);
        let required_mib = self.working_mib + READER_RESERVE_MIB;
        MemoryReadiness {
            ready: total_mib.is_some_and(|total| total >= self.minimum_total_mib)
                && available_mib.is_some_and(|available| available >= required_mib),
            available_mib,
            required_mib,
            total_mib,
            minimum_total_mib: self.minimum_total_mib,
        }
    }
}

#[cfg(target_vendor = "apple")]
fn system_memory() -> std::io::Result<(u64, u64)> {
    use std::{io::Error, mem, ptr, sync::OnceLock};

    static HOST: OnceLock<libc::mach_port_t> = OnceLock::new();
    unsafe extern "C" {
        fn mach_host_self() -> libc::mach_port_t;
    }
    let mut total = 0u64;
    let mut len = mem::size_of_val(&total);
    let mut stats = mem::MaybeUninit::<libc::vm_statistics64>::zeroed();
    let mut count = libc::HOST_VM_INFO64_COUNT;
    // The host port is reused for the lifetime of the process. All output buffers
    // are writable and sized for the corresponding OS structure.
    unsafe {
        if libc::sysctlbyname(
            c"hw.memsize".as_ptr(),
            (&mut total as *mut u64).cast(),
            &mut len,
            ptr::null_mut(),
            0,
        ) != 0
        {
            return Err(Error::last_os_error());
        }
        let host = *HOST.get_or_init(|| mach_host_self());
        let result = libc::host_statistics64(
            host,
            libc::HOST_VM_INFO64,
            stats.as_mut_ptr().cast(),
            &mut count,
        );
        let page_size = libc::sysconf(libc::_SC_PAGESIZE);
        if result != libc::KERN_SUCCESS
            || count != libc::HOST_VM_INFO64_COUNT
            || len != mem::size_of::<u64>()
            || total == 0
            || page_size <= 0
        {
            return Err(Error::other("macOS memory counters unavailable"));
        }
        let stats = stats.assume_init();
        // Speculative pages are included in the free count already. File-backed
        // pages are reclaimable under pressure, unlike anonymous inactive pages.
        let pages = u64::from(stats.free_count)
            + u64::from(stats.external_page_count)
                .saturating_sub(u64::from(stats.speculative_count));
        Ok((total, pages.saturating_mul(page_size as u64)))
    }
}

#[cfg(windows)]
fn system_memory() -> std::io::Result<(u64, u64)> {
    use std::{io::Error, mem};
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

    let mut status = MEMORYSTATUSEX {
        dwLength: mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // dwLength matches the initialized writable structure passed to Win32.
    if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 {
        return Err(Error::last_os_error());
    }
    Ok((
        status.ullTotalPhys,
        status.ullAvailPhys.min(status.ullAvailPageFile),
    ))
}

#[cfg(target_os = "linux")]
fn system_memory() -> std::io::Result<(u64, u64)> {
    use std::{fs, io::Error};

    let report = fs::read_to_string("/proc/meminfo")?;
    let field = |key: &str| {
        report
            .lines()
            .find_map(|line| {
                let mut parts = line.strip_prefix(key)?.split_whitespace();
                let value = parts.next()?.parse::<u64>().ok()?;
                (parts.next() == Some("kB")).then(|| value.saturating_mul(1024))
            })
            .ok_or_else(|| Error::other("Linux memory counters unavailable"))
    };
    Ok((field("MemTotal:")?, field("MemAvailable:")?))
}

#[cfg(not(any(target_vendor = "apple", windows, target_os = "linux")))]
fn system_memory() -> std::io::Result<(u64, u64)> {
    Err(std::io::Error::other("Memory counters unavailable"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        thread,
        time::{Duration, Instant},
    };

    #[test]
    fn reserves_memory_for_the_reader_and_fails_closed_without_measurement() {
        let small = ModelBudget {
            working_mib: 1536,
            minimum_total_mib: 0,
        };
        let large = ModelBudget {
            working_mib: 2560,
            minimum_total_mib: 6144,
        };
        assert!(!small.assess(None).ready);
        assert!(!small.assess(Some((4096 * MIB, 2559 * MIB))).ready);
        assert!(small.assess(Some((4096 * MIB, 2560 * MIB))).ready);
        assert!(!large.assess(Some((4096 * MIB, 4096 * MIB))).ready);
        assert!(large.assess(Some((8192 * MIB, 3584 * MIB))).ready);
    }

    #[test]
    fn requested_model_signals_background_work_and_takes_the_permit_next() {
        let governor = ResourceGovernor::new();
        let optional = governor.optional().unwrap();
        assert!(governor.optional().is_err());
        thread::scope(|scope| {
            let reader = scope.spawn(|| {
                let _model = governor.requested_model();
            });
            let deadline = Instant::now() + Duration::from_secs(2);
            while !optional.should_yield() && Instant::now() < deadline {
                thread::yield_now();
            }
            let yielded = optional.should_yield();
            drop(optional);
            reader.join().unwrap();
            assert!(yielded);
        });
        assert!(governor.optional().is_ok());
    }
}
