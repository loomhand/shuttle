//! A Windows Job object around a spawned child, so the child and every process it starts die together.

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
use windows::core::PCWSTR;

/// A job that holds one child's process tree and kills it when dropped.
pub(crate) struct Job(HANDLE);

// SAFETY: a job handle may be used and closed from any thread.
unsafe impl Send for Job {}
// SAFETY: as above; the methods only pass the handle to thread-safe calls.
unsafe impl Sync for Job {}

impl Job {
    /// Puts `child`, a process handle from a spawn just now, into a new job that kills its tree on close.
    pub(crate) fn hold(child: Option<std::os::windows::io::RawHandle>) -> Result<Self, String> {
        let child = child.ok_or("the child has no process handle")?;
        // SAFETY: no attributes and no name: CreateJobObjectW returns a fresh handle owned by `job` below.
        let job = Self(
            unsafe { CreateJobObjectW(None, PCWSTR::null()) }
                .map_err(|e| format!("create a job: {e}"))?,
        );
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `limits` is a JOBOBJECT_EXTENDED_LIMIT_INFORMATION of the size passed.
        unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                u32::try_from(size_of_val(&limits)).unwrap_or(u32::MAX),
            )
        }
        .map_err(|e| format!("set the job's limits: {e}"))?;
        // SAFETY: `child` is the live process handle of the child just spawned.
        unsafe { AssignProcessToJobObject(job.0, HANDLE(child)) }
            .map_err(|e| format!("put the child in its job: {e}"))?;
        Ok(job)
    }

    /// Kills every process in the job.
    pub(crate) fn kill(&self) {
        // SAFETY: `self.0` is the job handle this value owns.
        let _ = unsafe { TerminateJobObject(self.0, 1) };
    }

    /// Lets the job's processes outlive it: drops the kill-on-close limit, then closes the handle.
    pub(crate) fn release(self) {
        let limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        // SAFETY: `limits` is a JOBOBJECT_EXTENDED_LIMIT_INFORMATION of the size passed.
        let _ = unsafe {
            SetInformationJobObject(
                self.0,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                u32::try_from(size_of_val(&limits)).unwrap_or(u32::MAX),
            )
        };
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: `self.0` is the job handle this value owns, closed once; kill-on-close then ends its processes.
        let _ = unsafe { CloseHandle(self.0) };
    }
}
