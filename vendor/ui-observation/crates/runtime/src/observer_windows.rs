//! Observer-side Windows process creation with an explicit handle allowlist.

use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs::File,
    io,
    mem::size_of,
    os::windows::{
        ffi::OsStrExt,
        io::{FromRawHandle, OwnedHandle},
        process::ExitStatusExt,
    },
    path::Path,
    process::ExitStatus,
};
use ui_observation_protocol::FrameReader;
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, SetHandleInformation, GENERIC_READ, GENERIC_WRITE, HANDLE,
        HANDLE_FLAG_INHERIT, WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    Security::SECURITY_ATTRIBUTES,
    Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    },
    System::{
        Pipes::CreatePipe,
        Threading::{
            CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
            InitializeProcThreadAttributeList, TerminateProcess, UpdateProcThreadAttribute,
            WaitForSingleObject, EXTENDED_STARTUPINFO_PRESENT, INFINITE, PROCESS_INFORMATION,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST, STARTF_USESTDHANDLES, STARTUPINFOEXW,
        },
    },
};

/// A Windows command launched with the observation pipe and deterministic NUL
/// standard streams as its complete inherited-handle allowlist.
#[derive(Clone, Debug)]
pub struct WindowsObservedCommand {
    program: OsString,
    arguments: Vec<OsString>,
    current_directory: Option<OsString>,
    environment: BTreeMap<OsString, Option<OsString>>,
    capture_stdio: bool,
    observation_pipe_bytes: u32,
}

impl WindowsObservedCommand {
    #[must_use]
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        Self {
            program: program.as_ref().to_owned(),
            arguments: Vec::new(),
            current_directory: None,
            environment: BTreeMap::new(),
            capture_stdio: false,
            observation_pipe_bytes: 65536,
        }
    }

    pub fn arg(&mut self, argument: impl AsRef<OsStr>) -> &mut Self {
        self.arguments.push(argument.as_ref().to_owned());
        self
    }

    pub fn args<I, S>(&mut self, arguments: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.arguments
            .extend(arguments.into_iter().map(|value| value.as_ref().to_owned()));
        self
    }

    pub fn current_dir(&mut self, directory: impl AsRef<Path>) -> &mut Self {
        self.current_directory = Some(directory.as_ref().as_os_str().to_owned());
        self
    }

    pub fn env(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
        self.environment
            .insert(key.as_ref().to_owned(), Some(value.as_ref().to_owned()));
        self
    }

    pub fn env_remove(&mut self, key: impl AsRef<OsStr>) -> &mut Self {
        self.environment.insert(key.as_ref().to_owned(), None);
        self
    }

    /// Bounded anonymous transport buffering for bursty producers. This creates
    /// no pathname, archive, or observation file and changes no producer record.
    pub fn observation_pipe_bytes(&mut self, bytes: u32) -> io::Result<&mut Self> {
        if !(4096..=16*1024*1024).contains(&bytes) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput,"observation pipe capacity must be 4096..16777216 bytes"));
        }
        self.observation_pipe_bytes=bytes;
        Ok(self)
    }

    /// Capture public stdout/stderr through separate anonymous pipes. The caller
    /// must take and drain both readers concurrently before waiting for exit.
    pub fn capture_stdio(&mut self) -> &mut Self {
        self.capture_stdio = true;
        self
    }

    /// Starts the command with a fresh anonymous observation pipe. The Windows
    /// `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` contains exactly its write handle.
    pub fn spawn(&self, nonce: &str) -> io::Result<ObservedWindowsChild> {
        if !ui_observation_protocol::is_nonce(nonce) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "nonce must be 128-bit lowercase hexadecimal",
            ));
        }
        spawn(self, nonce)
    }
}

/// Observer-owned Windows process and its one-way framed application stream.
pub struct ObservedWindowsChild {
    process: OwnedHandle,
    process_id: u32,
    pub stream: FrameReader<File>,
    stdout: Option<File>,
    stderr: Option<File>,
    telemetry: std::sync::Arc<super::queue_windows::QueueTelemetry>,
}

impl ObservedWindowsChild {
    pub fn take_stdout(&mut self) -> Option<File> { self.stdout.take() }
    pub fn take_stderr(&mut self) -> Option<File> { self.stderr.take() }

    #[must_use]
    pub fn id(&self) -> u32 {
        self.process_id
    }

    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        // SAFETY: `process` remains owned for the duration of the wait.
        let result = unsafe { WaitForSingleObject(self.process.as_raw_handle().cast(), INFINITE) };
        if result != WAIT_OBJECT_0 {
            return Err(io::Error::last_os_error());
        }
        self.exit_status()
    }

    pub fn kill(&mut self) -> io::Result<()> {
        // SAFETY: `process` is a live process handle owned by this value.
        if unsafe { TerminateProcess(self.process.as_raw_handle().cast(), 1) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// Separates the independently owned process and observation stream.
    ///
    /// This is useful when a reader thread must drain the stream while the
    /// controlling thread waits for process termination.
    #[must_use]
    pub fn into_parts(self) -> (ObservedWindowsProcess, FrameReader<File>) {
        (
            ObservedWindowsProcess {
                process: self.process,
                process_id: self.process_id,
                telemetry: self.telemetry,
            },
            self.stream,
        )
    }

    fn exit_status(&self) -> io::Result<ExitStatus> {
        let mut code = 0_u32;
        // SAFETY: output points to initialized writable storage and the process
        // handle is live.
        if unsafe { GetExitCodeProcess(self.process.as_raw_handle().cast(), &mut code) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(ExitStatus::from_raw(code))
        }
    }
}

/// Process half returned by [`ObservedWindowsChild::into_parts`].
pub struct ObservedWindowsProcess {
    telemetry: std::sync::Arc<super::queue_windows::QueueTelemetry>,
    process: OwnedHandle,
    process_id: u32,
}

impl ObservedWindowsProcess {
    pub fn queue_snapshot(&self) -> super::QueueSnapshot { self.telemetry.snapshot() }
    /// Read live anonymous counters independently of the process owner or GUI.
    /// The returned probe retains the pinned mapping and exposes no controls.
    pub fn queue_probe(&self) -> impl Fn() -> super::QueueSnapshot + Send + Sync + 'static {
        let telemetry = self.telemetry.clone();
        move || telemetry.snapshot()
    }
    #[must_use]
    pub fn id(&self) -> u32 {
        self.process_id
    }

    /// Poll process health without blocking the observer's GUI.
    pub fn try_wait(&self) -> io::Result<Option<ExitStatus>> {
        // SAFETY: the handle is owned by this process object.
        match unsafe { WaitForSingleObject(self.process.as_raw_handle().cast(), 0) } {
            WAIT_OBJECT_0 => self.exit_status().map(Some),
            WAIT_TIMEOUT => Ok(None),
            _ => Err(io::Error::last_os_error()),
        }
    }

    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        // SAFETY: `process` remains owned for the duration of the wait.
        let result = unsafe { WaitForSingleObject(self.process.as_raw_handle().cast(), INFINITE) };
        if result != WAIT_OBJECT_0 {
            return Err(io::Error::last_os_error());
        }
        self.exit_status()
    }

    pub fn kill(&mut self) -> io::Result<()> {
        // SAFETY: `process` is a live process handle owned by this value.
        if unsafe { TerminateProcess(self.process.as_raw_handle().cast(), 1) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn exit_status(&self) -> io::Result<ExitStatus> {
        let mut code = 0_u32;
        // SAFETY: output points to initialized writable storage and the process
        // handle is live.
        if unsafe { GetExitCodeProcess(self.process.as_raw_handle().cast(), &mut code) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(ExitStatus::from_raw(code))
        }
    }
}

use std::os::windows::io::AsRawHandle;

fn spawn(command: &WindowsObservedCommand, nonce: &str) -> io::Result<ObservedWindowsChild> {
    let mut read_handle: HANDLE = std::ptr::null_mut();
    let mut write_handle: HANDLE = std::ptr::null_mut();
    let security = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: 1,
    };
    // SAFETY: both outputs and the security descriptor are valid for this call.
    if unsafe { CreatePipe(&mut read_handle, &mut write_handle, &security, command.observation_pipe_bytes) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // Establish local RAII immediately. Each raw handle now has exactly one owner.
    // SAFETY: CreatePipe returned two new, distinct, live handles.
    let read = unsafe { OwnedHandle::from_raw_handle(read_handle.cast()) };
    // SAFETY: as above, for the write end.
    let write = unsafe { OwnedHandle::from_raw_handle(write_handle.cast()) };
    // SAFETY: read is live; clear exactly the inheritance flag.
    if unsafe { SetHandleInformation(read.as_raw_handle().cast(), HANDLE_FLAG_INHERIT, 0) } == 0 {
        return Err(io::Error::last_os_error());
    }

    let mut attribute_size = 0_usize;
    // The documented sizing call fails with ERROR_INSUFFICIENT_BUFFER while
    // returning the required byte count.
    // SAFETY: a null list is required for the sizing call.
    unsafe {
        InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut attribute_size);
    }
    if attribute_size == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut attribute_storage = vec![0_u8; attribute_size];
    let attribute_list = attribute_storage.as_mut_ptr().cast();
    // SAFETY: storage has the exact requested size and remains stable until
    // after CreateProcessW returns.
    if unsafe { InitializeProcThreadAttributeList(attribute_list, 1, 0, &mut attribute_size) } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let attributes = AttributeList(attribute_list);
    let null_name = wide_nul(OsStr::new("NUL"))?;
    // SAFETY: name and security descriptor remain live for the call. The NUL
    // device prevents console/test output from contaminating observation data.
    let null_handle = unsafe {
        CreateFileW(
            null_name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            &security,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    if null_handle.is_null() || null_handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateFileW returned a new live handle with one local owner.
    let null_device = unsafe { OwnedHandle::from_raw_handle(null_handle.cast()) };
    let telemetry = std::sync::Arc::new(super::queue_windows::QueueTelemetry::create(super::DEFAULT_CAPACITY)?);
    let stdout = command.capture_stdio.then(anonymous_output_pipe).transpose()?;
    let stderr = command.capture_stdio.then(anonymous_output_pipe).transpose()?;
    let mut inherited = vec![
        telemetry.handle() as *mut core::ffi::c_void,
        write.as_raw_handle().cast::<core::ffi::c_void>(),
        null_device.as_raw_handle().cast::<core::ffi::c_void>(),
    ];
    for pipe in [&stdout, &stderr].into_iter().flatten() {
        inherited.push(pipe.1.as_raw_handle().cast::<core::ffi::c_void>());
    }
    // SAFETY: the attribute list is initialized and `inherited` lives through
    // process creation. It contains only the observation writer and NUL stdio.
    if unsafe {
        UpdateProcThreadAttribute(
            attributes.0,
            0,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
            inherited.as_ptr().cast(),
            size_of_val(inherited.as_slice()),
            std::ptr::null_mut(),
            std::ptr::null(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }

    let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = null_device.as_raw_handle().cast();
    startup.StartupInfo.hStdOutput = stdout.as_ref().map_or(null_device.as_raw_handle(), |pipe| pipe.1.as_raw_handle()).cast();
    startup.StartupInfo.hStdError = stderr.as_ref().map_or(null_device.as_raw_handle(), |pipe| pipe.1.as_raw_handle()).cast();
    startup.lpAttributeList = attributes.0;
    let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let application = wide_nul(&command.program)?;
    let mut command_line = command_line(command)?;
    let current_directory = command
        .current_directory
        .as_ref()
        .map(|value| wide_nul(value))
        .transpose()?;

    // Add the run-owned counter mapping to the same explicit inheritance allowlist.
    let mut child_command = command.clone();
    child_command.env("UI_OBSERVATION_V1_QUEUE_HANDLE", telemetry.handle().to_string());
    let mut environment = environment_block(&child_command, nonce, write.as_raw_handle() as usize)?;

    // SAFETY: all pointers refer to live NUL-terminated/mutable buffers for the
    // duration of the call; the startup attribute list is initialized.
    let created = unsafe {
        CreateProcessW(
            application.as_ptr(),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            EXTENDED_STARTUPINFO_PRESENT | 0x0000_0400, // CREATE_UNICODE_ENVIRONMENT
            environment.as_mut_ptr().cast(),
            current_directory
                .as_ref()
                .map_or(std::ptr::null(), |value| value.as_ptr()),
            (&startup.StartupInfo) as *const _,
            &mut process,
        )
    };
    drop(attributes);
    if created == 0 {
        return Err(io::Error::last_os_error());
    }
    drop(write);
    // SAFETY: the thread handle is returned solely to this function and is not
    // otherwise used after process creation.
    unsafe { CloseHandle(process.hThread) };
    // SAFETY: CreateProcessW returned a new process handle now solely owned here.
    let process_handle = unsafe { OwnedHandle::from_raw_handle(process.hProcess.cast()) };
    Ok(ObservedWindowsChild {
        process: process_handle,
        process_id: process.dwProcessId,
        stream: FrameReader::new(File::from(read)),
        stdout: stdout.map(|(read, _write)| File::from(read)),
        stderr: stderr.map(|(read, _write)| File::from(read)),
        telemetry,
    })
}

/// Read ends never enter the child's explicit inheritance allowlist.
fn anonymous_output_pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
    let mut read = std::ptr::null_mut();
    let mut write = std::ptr::null_mut();
    let security = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(), bInheritHandle: 1,
    };
    // SAFETY: initialized outputs and descriptor live for the call.
    if unsafe { CreatePipe(&mut read, &mut write, &security, 65536) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: these two handles are fresh, distinct, and exclusively owned.
    let read = unsafe { OwnedHandle::from_raw_handle(read.cast()) };
    let write = unsafe { OwnedHandle::from_raw_handle(write.cast()) };
    if unsafe { SetHandleInformation(read.as_raw_handle().cast(), HANDLE_FLAG_INHERIT, 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((read, write))
}

struct AttributeList(windows_sys::Win32::System::Threading::LPPROC_THREAD_ATTRIBUTE_LIST);

impl Drop for AttributeList {
    fn drop(&mut self) {
        // SAFETY: this wrapper is created only after successful initialization.
        unsafe { DeleteProcThreadAttributeList(self.0) };
    }
}

fn wide_nul(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut wide: Vec<u16> = value.encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Windows process strings cannot contain NUL",
        ));
    }
    wide.push(0);
    Ok(wide)
}

fn command_line(command: &WindowsObservedCommand) -> io::Result<Vec<u16>> {
    let mut line = quote_argument(&command.program)?;
    for argument in &command.arguments {
        line.push(' ' as u16);
        line.extend(quote_argument(argument)?);
    }
    line.push(0);
    Ok(line)
}

// Implements the CommandLineToArgvW/MSVCRT backslash-before-quote convention.
fn quote_argument(value: &OsStr) -> io::Result<Vec<u16>> {
    let raw: Vec<u16> = value.encode_wide().collect();
    if raw.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "argument contains NUL",
        ));
    }
    let mut output = vec!['"' as u16];
    let mut slashes = 0_usize;
    for unit in raw {
        if unit == '\\' as u16 {
            slashes += 1;
        } else if unit == '"' as u16 {
            output.extend(std::iter::repeat_n('\\' as u16, slashes * 2 + 1));
            output.push(unit);
            slashes = 0;
        } else {
            output.extend(std::iter::repeat_n('\\' as u16, slashes));
            output.push(unit);
            slashes = 0;
        }
    }
    output.extend(std::iter::repeat_n('\\' as u16, slashes * 2));
    output.push('"' as u16);
    Ok(output)
}

fn environment_block(
    command: &WindowsObservedCommand,
    nonce: &str,
    write_handle: usize,
) -> io::Result<Vec<u16>> {
    let mut environment: BTreeMap<OsString, OsString> = std::env::vars_os().collect();
    for (key, value) in &command.environment {
        match value {
            Some(value) => {
                environment.insert(key.clone(), value.clone());
            }
            None => {
                environment.remove(key);
            }
        }
    }
    environment.remove(OsStr::new("UI_OBSERVATION_V1_FD"));
    environment.insert(
        OsString::from("UI_OBSERVATION_V1_HANDLE"),
        OsString::from(write_handle.to_string()),
    );
    environment.insert(
        OsString::from("UI_OBSERVATION_V1_NONCE"),
        OsString::from(nonce),
    );
    let mut entries: Vec<(OsString, OsString)> = environment.into_iter().collect();
    entries.sort_by(|left, right| {
        left.0
            .to_string_lossy()
            .to_uppercase()
            .cmp(&right.0.to_string_lossy().to_uppercase())
    });
    let mut block = Vec::new();
    for (key, value) in entries {
        let key: Vec<u16> = key.encode_wide().collect();
        let value: Vec<u16> = value.encode_wide().collect();
        // `std::env` exposes cmd.exe's per-drive current-directory pseudo
        // variables (for example `=C:`). They are not ordinary variables and
        // cannot be emitted by this builder's `name=value` representation.
        if key.first() == Some(&('=' as u16)) {
            continue;
        }
        if key.is_empty() || key.contains(&0) || key.contains(&('=' as u16)) || value.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid Windows environment entry",
            ));
        }
        block.extend(key);
        block.push('=' as u16);
        block.extend(value);
        block.push(0);
    }
    block.push(0);
    Ok(block)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    #[test]
    fn captured_stdio_child_helper() {
        if std::env::var_os("UI_OBSERVATION_STDIO_FIXTURE").is_none() { return; }
        println!("public-stdout-fixture");
        eprintln!("public-stderr-fixture");
        let runtime=crate::ObservationRuntime::from_inherited_v1();
        assert_eq!(runtime.finish(0,ui_observation_protocol::ElementKey::parse("window.main").unwrap()),crate::RecordStatus::Recorded);
    }
    #[test]
    fn independent_stdio_pipes_do_not_contaminate_the_observation_stream() {
        let nonce="0123456789abcdef0123456789abcdef";
        let mut command=WindowsObservedCommand::new(std::env::current_exe().unwrap());
        command.args(["--exact","observer_windows::tests::captured_stdio_child_helper","--nocapture"])
            .env("UI_OBSERVATION_STDIO_FIXTURE","1").capture_stdio();
        let mut child=command.spawn(nonce).unwrap();
        let stdout=child.take_stdout().unwrap();let stderr=child.take_stderr().unwrap();
        let drain=|input:File|std::thread::spawn(move||{let mut bytes=Vec::new();input.take(65537).read_to_end(&mut bytes).unwrap();assert!(bytes.len()<=65536);bytes});
        let out=drain(stdout);let err=drain(stderr);
        let (mut process,mut observations)=child.into_parts();let mut count=0;
        while let Some(record)=observations.read().unwrap(){assert_eq!(record.run_nonce,nonce);count+=1;}
        assert!(process.wait().unwrap().success());assert_eq!(count,1);
        let out=String::from_utf8(out.join().unwrap()).unwrap();let err=String::from_utf8(err.join().unwrap()).unwrap();
        assert!(out.contains("public-stdout-fixture"));assert!(!out.contains("public-stderr-fixture"));
        assert!(err.contains("public-stderr-fixture"));assert!(!err.contains("public-stdout-fixture"));
    }
}
