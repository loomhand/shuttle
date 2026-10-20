//! A Windows Job object around a spawned child, so the child and every process it starts die together.

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
use windows::Win32::System::Pipes::GetNamedPipeServerProcessId;
use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
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
        Self::with(child, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE)
    }

    /// Like [`Job::hold`] for `child` alone: the processes it starts stay outside the job and outlive it.
    pub(crate) fn hold_alone(
        child: Option<std::os::windows::io::RawHandle>,
    ) -> Result<Self, String> {
        Self::with(
            child,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK,
        )
    }

    fn with(
        child: Option<std::os::windows::io::RawHandle>,
        flags: JOB_OBJECT_LIMIT,
    ) -> Result<Self, String> {
        let child = child.ok_or("the child has no process handle")?;
        // SAFETY: no attributes and no name: CreateJobObjectW returns a fresh handle owned by `job` below.
        let job = Self(
            unsafe { CreateJobObjectW(None, PCWSTR::null()) }
                .map_err(|e| format!("create a job: {e}"))?,
        );
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = flags;
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

    /// Whether the process serving the client end `pipe` is in this job; an error names that process.
    pub(crate) fn serves(&self, pipe: HANDLE) -> Result<(), String> {
        let mut pid = 0;
        // SAFETY: `pipe` is a live client handle of a connected pipe; `pid` receives the server's id.
        unsafe { GetNamedPipeServerProcessId(pipe, &raw mut pid) }
            .map_err(|e| format!("read the pipe's server process: {e}"))?;
        // SAFETY: OpenProcess takes plain values; the handle is closed below.
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
            .map_err(|e| format!("open the pipe's server process {pid}: {e}"))?;
        let mut inside = false.into();
        // SAFETY: both handles are live; `inside` receives the answer.
        let asked = unsafe { IsProcessInJob(process, Some(self.0), &raw mut inside) };
        // SAFETY: `process` was opened above and is closed once.
        let _ = unsafe { CloseHandle(process) };
        asked.map_err(|e| format!("check the pipe's server process {pid}: {e}"))?;
        if inside.as_bool() {
            Ok(())
        } else {
            Err(format!(
                "process {pid} serves the pipe, outside the service's process tree"
            ))
        }
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
