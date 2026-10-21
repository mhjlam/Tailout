use anyhow::{Context, Result, bail};
use std::{
    io::{BufRead, BufReader, Read},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

#[derive(Clone, Default)]
pub struct Cancel(pub Arc<AtomicBool>);
impl Cancel {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            bail!("Cancelled");
        }
        Ok(())
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// One complete record from FFmpeg's machine-readable progress stream.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FfmpegProgress {
    pub seconds: Option<f64>,
    pub frame: Option<u64>,
    pub bytes: Option<u64>,
    pub fps: Option<f64>,
    pub speed: Option<f64>,
    pub finished: bool,
}
impl FfmpegProgress {
    fn line(&mut self, line: &str) -> Option<Self> {
        let (key, value) = line.trim().split_once('=')?;
        let finite = |text: &str| {
            text.trim()
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite() && *v >= 0.0)
        };
        match key {
            "out_time_us" | "out_time_ms" => {
                // FFmpeg's legacy out_time_ms field is also in microseconds.
                if let Some(value) = finite(value) {
                    self.seconds = Some(value / 1_000_000.0);
                }
            }
            "out_time" if self.seconds.is_none() => {
                self.seconds = crate::model::parse_time(value).ok()
            }
            "frame" => self.frame = value.trim().parse().ok(),
            "total_size" => self.bytes = value.trim().parse().ok(),
            "fps" => self.fps = finite(value),
            "speed" => self.speed = finite(value.trim_end_matches('x')).filter(|v| *v > 0.0),
            "progress" => {
                self.finished = value == "end";
                return Some(std::mem::take(self));
            }
            _ => {}
        }
        None
    }
}

#[derive(Debug)]
pub struct Output {
    pub stdout: Vec<u8>,
    pub stderr: String,
}

struct Running {
    child: Child,
    job: Option<Job>,
}
impl Running {
    fn stop(&mut self) {
        self.job.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        self.stop();
    }
}

// All backend commands use argument arrays, concurrent pipe readers, cancellation
// and bounded capture. No shell is involved, including for filenames and filters.
pub fn run(
    command: &mut Command,
    cancel: &Cancel,
    progress: bool,
    on_progress: &mut dyn FnMut(f64),
) -> Result<Output> {
    run_with_progress(command, cancel, progress, &mut |record| {
        if let Some(seconds) = record.seconds {
            on_progress(seconds);
        }
    })
}

pub fn run_with_progress(
    command: &mut Command,
    cancel: &Cancel,
    progress: bool,
    on_progress: &mut dyn FnMut(FfmpegProgress),
) -> Result<Output> {
    cancel.check()?;
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let job = Job::new()?;
    let mut child = command.spawn().with_context(|| {
        format!(
            "Cannot start {:?}. Install FFmpeg/FFprobe or place them beside the app.",
            command.get_program()
        )
    })?;
    if let Err(error) = job.assign(&child) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    let stdout = child.stdout.take().context("Missing stdout")?;
    let stderr = child.stderr.take().context("Missing stderr")?;
    let mut running = Running {
        child,
        job: Some(job),
    };
    let overflow = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    let exceeded = overflow.clone();
    let out_thread = thread::spawn(move || -> std::io::Result<Vec<u8>> {
        if progress {
            let mut record = FfmpegProgress::default();
            for line in BufReader::new(stdout).lines() {
                if let Some(record) = record.line(&line?) {
                    let _ = tx.send(record);
                }
            }
            Ok(Vec::new())
        } else {
            capture(stdout, 128 * 1024 * 1024, &exceeded)
        }
    });
    let exceeded = overflow.clone();
    let err_thread = thread::spawn(move || capture(stderr, 16 * 1024 * 1024, &exceeded));
    let status = loop {
        for seconds in rx.try_iter() {
            on_progress(seconds);
        }
        if cancel.is_cancelled() || overflow.load(Ordering::Relaxed) {
            running.stop();
            break running.child.wait()?;
        }
        if let Some(status) = running.child.try_wait()? {
            running.job.take();
            break status;
        }
        thread::sleep(Duration::from_millis(30));
    };
    let stdout = out_thread
        .join()
        .map_err(|_| anyhow::anyhow!("stdout reader failed"))??;
    let stderr = err_thread
        .join()
        .map_err(|_| anyhow::anyhow!("stderr reader failed"))??;
    for seconds in rx.try_iter() {
        on_progress(seconds);
    }
    cancel.check()?;
    if overflow.load(Ordering::Relaxed) {
        bail!("Backend output exceeded the analysis memory limit.");
    }
    let stderr = String::from_utf8_lossy(&stderr).into_owned();
    if !status.success() {
        let tail: String = stderr
            .chars()
            .rev()
            .take(6000)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        bail!("Backend failed ({status}):\n{tail}");
    }
    Ok(Output { stdout, stderr })
}

fn capture(mut input: impl Read, limit: usize, overflow: &AtomicBool) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = [0; 16384];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        if output.len() + count <= limit {
            output.extend_from_slice(&buffer[..count]);
        } else {
            overflow.store(true, Ordering::Relaxed);
        }
    }
    Ok(output)
}

#[cfg(windows)]
struct Job(windows_sys::Win32::Foundation::HANDLE);
#[cfg(windows)]
impl Job {
    fn new() -> Result<Self> {
        use windows_sys::Win32::{Foundation::CloseHandle, System::JobObjects::*};
        // SAFETY: initialized C layouts and valid pointers; handle is owned here.
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                return Err(std::io::Error::last_os_error().into());
            }
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                std::mem::size_of_val(&limits) as u32,
            ) == 0
            {
                let error = std::io::Error::last_os_error();
                CloseHandle(handle);
                return Err(error.into());
            }
            Ok(Self(handle))
        }
    }
    fn assign(&self, child: &Child) -> Result<()> {
        use std::os::windows::io::AsRawHandle;
        // SAFETY: both handles are live and owned for the duration of the call.
        if unsafe {
            windows_sys::Win32::System::JobObjects::AssignProcessToJobObject(
                self.0,
                child.as_raw_handle(),
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
}
#[cfg(windows)]
impl Drop for Job {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}
#[cfg(not(windows))]
struct Job;
#[cfg(not(windows))]
impl Job {
    fn new() -> Result<Self> {
        Ok(Self)
    }
    fn assign(&self, _: &Child) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn progress_records_are_emitted_live_and_do_not_reuse_unavailable_fields() {
        let mut parser = FfmpegProgress::default();
        for line in [
            "frame= 60",
            "out_time_us=2000000",
            "out_time_ms=2000000",
            "out_time=00:00:02.000000",
            "total_size=4096",
            "fps=30.0",
            "speed=0.5x",
        ] {
            assert!(parser.line(line).is_none());
        }
        let record = parser.line("progress=continue").unwrap();
        assert_eq!(record.seconds, Some(2.0));
        assert_eq!(record.frame, Some(60));
        assert_eq!(record.bytes, Some(4096));
        assert_eq!(record.speed, Some(0.5));
        assert!(!record.finished);
        for line in [
            "out_time_us=N/A",
            "frame=N/A",
            "total_size=N/A",
            "fps=NaN",
            "speed=N/A",
        ] {
            parser.line(line);
        }
        let end = parser.line("progress=end").unwrap();
        assert!(end.finished);
        assert!(
            end.seconds.is_none()
                && end.frame.is_none()
                && end.bytes.is_none()
                && end.speed.is_none()
                && end.fps.is_none()
        );
    }
    #[test]
    fn progress_supports_legacy_and_text_timestamps_and_rejects_invalid_values() {
        for (line, seconds) in [
            ("out_time_ms=1250000", 1.25),
            ("out_time=01:02:03.250000", 3723.25),
        ] {
            let mut parser = FfmpegProgress::default();
            parser.line(line);
            assert_eq!(
                parser.line("progress=continue").unwrap().seconds,
                Some(seconds)
            );
        }
        let mut parser = FfmpegProgress::default();
        for line in [
            "out_time_us=-100000",
            "out_time_ms=NaN",
            "out_time=N/A",
            "speed=inf",
            "fps=-1",
        ] {
            parser.line(line);
        }
        let record = parser.line("progress=continue").unwrap();
        assert!(record.seconds.is_none() && record.speed.is_none() && record.fps.is_none());
    }
}
