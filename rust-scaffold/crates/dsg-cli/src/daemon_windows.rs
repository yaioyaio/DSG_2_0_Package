//! SCM-owned, single-instance local named pipe. Only a parsed route crosses
//! the broker boundary. No client bytes or impersonation token reach the handler.
use super::daemon_http::{self, Reply, Route, Transport};
use super::daemon_windows_config::WindowsBrokerConfig;
use std::ffi::c_void;
use std::io;
use std::mem::size_of;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, ConvertStringSidToSidW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::*;
use windows_sys::Win32::Storage::FileSystem::{
    ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_READ_ATTRIBUTES, FILE_READ_DATA,
    FILE_WRITE_DATA, PIPE_ACCESS_DUPLEX, SYNCHRONIZE,
};
use windows_sys::Win32::System::Pipes::*;
use windows_sys::Win32::System::Services::*;
use windows_sys::Win32::System::Threading::{GetCurrentThread, OpenThreadToken};

type Handler = fn(&WindowsBrokerConfig, Route) -> Reply;
static SERVICE: OnceLock<(WindowsBrokerConfig, Handler)> = OnceLock::new();
static STOP: AtomicBool = AtomicBool::new(false);
// Pessimistic until service_main has finished and reported its terminal state.
static RESULT: AtomicU32 = AtomicU32::new(ERROR_GEN_FAILURE);
const POLL: Duration = Duration::from_millis(20);

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

fn checked(success: i32) -> io::Result<()> {
    if success == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

struct Local(*mut c_void);
impl Drop for Local {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}

pub(super) fn run(config: WindowsBrokerConfig, handle: Handler) -> io::Result<()> {
    let mut name = wide(config.service_name());
    SERVICE
        .set((config, handle))
        .map_err(|_| io::ErrorKind::AlreadyExists)?;
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: name.as_mut_ptr(),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW {
            lpServiceName: null_mut(),
            lpServiceProc: None,
        },
    ];
    checked(unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) })?;
    match RESULT.load(Ordering::Acquire) {
        ERROR_SUCCESS => Ok(()),
        code => Err(io::Error::from_raw_os_error(code as i32)),
    }
}

unsafe extern "system" fn control(code: u32, _: u32, _: *mut c_void, _: *mut c_void) -> u32 {
    match code {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
            STOP.store(true, Ordering::Release);
            ERROR_SUCCESS
        }
        SERVICE_CONTROL_INTERROGATE => ERROR_SUCCESS,
        _ => ERROR_CALL_NOT_IMPLEMENTED,
    }
}

unsafe extern "system" fn service_main(_: u32, _: *mut *mut u16) {
    let Some((config, handle)) = SERVICE.get() else {
        return;
    };
    let name = wide(config.service_name());
    let status = unsafe { RegisterServiceCtrlHandlerExW(name.as_ptr(), Some(control), null()) };
    if status.is_null() {
        RESULT.store(unsafe { GetLastError() }, Ordering::Release);
        return;
    }
    let result = serve(status, config, *handle);
    let mut code = match result {
        Ok(()) => ERROR_SUCCESS,
        Err(e) => e.raw_os_error().map_or(ERROR_GEN_FAILURE, |n| n as u32),
    };
    if let Err(e) = report(status, SERVICE_STOPPED, code) {
        code = e.raw_os_error().map_or(ERROR_GEN_FAILURE, |n| n as u32);
    }
    RESULT.store(code, Ordering::Release);
}

fn report(handle: SERVICE_STATUS_HANDLE, state: u32, error: u32) -> io::Result<()> {
    let pending = matches!(state, SERVICE_START_PENDING | SERVICE_STOP_PENDING);
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: if state == SERVICE_RUNNING {
            SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN
        } else {
            0
        },
        dwWin32ExitCode: error,
        dwServiceSpecificExitCode: 0,
        dwCheckPoint: u32::from(pending),
        dwWaitHint: if pending { 5000 } else { 0 },
    };
    checked(unsafe { SetServiceStatus(handle, &status) })
}

struct Listener {
    pipe: Handle,
    allowed: Vec<Local>,
}

impl Listener {
    fn create(config: &WindowsBrokerConfig) -> io::Result<Self> {
        // LocalSystem owns the endpoint. Only SYSTEM receives server creation
        // rights. CreateFile can request FILE_READ_ATTRIBUTES in addition to
        // the client's explicit data access and synchronization rights.
        let client_access = FILE_READ_DATA | FILE_WRITE_DATA | FILE_READ_ATTRIBUTES | SYNCHRONIZE;
        let mut acl = String::from("O:SYD:P(A;;GA;;;SY)");
        let mut allowed = Vec::new();
        for sid in config.client_sids() {
            let mut parsed = null_mut();
            checked(unsafe { ConvertStringSidToSidW(wide(sid.as_str()).as_ptr(), &mut parsed) })?;
            allowed.push(Local(parsed));
            acl.push_str(&format!("(A;;0x{client_access:08x};;;{})", sid.as_str()));
        }
        let mut descriptor = null_mut();
        checked(unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide(&acl).as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                null_mut(),
            )
        })?;
        let descriptor = Local(descriptor);
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: 0,
        };
        let pipe = unsafe {
            CreateNamedPipeW(
                wide(config.listener()).as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                16384,
                8192,
                0,
                &attributes,
            )
        };
        if pipe == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            pipe: Handle(pipe),
            allowed,
        })
    }

    fn accept(&self) -> io::Result<bool> {
        while !STOP.load(Ordering::Acquire) {
            if unsafe { ConnectNamedPipe(self.pipe.0, null_mut()) } == 0 {
                match unsafe { GetLastError() } {
                    ERROR_PIPE_CONNECTED => return Ok(true),
                    ERROR_PIPE_LISTENING => (),
                    ERROR_NO_DATA => self.disconnect()?,
                    code => return Err(io::Error::from_raw_os_error(code as i32)),
                }
            }
            // In NOWAIT mode a successful call makes the instance available.
            std::thread::sleep(POLL);
        }
        Ok(false)
    }

    fn disconnect(&self) -> io::Result<()> {
        checked(unsafe { DisconnectNamedPipe(self.pipe.0) })
    }

    fn user_allowed(&self) -> io::Result<()> {
        checked(unsafe { ImpersonateNamedPipeClient(self.pipe.0) })?;
        let scope = Impersonation;
        let mut token = null_mut();
        checked(unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) })?;
        let token = Handle(token);
        // Get a token handle while impersonating; return to the service identity
        // before querying it or invoking application code.
        drop(scope);
        // TokenUser has one SID (SECURITY_MAX_SID_SIZE is 68 bytes). Allocate
        // aligned space on the stack, with room for TokenUser on x86 and x64.
        let mut storage = [0usize; 32];
        let mut returned = 0;
        checked(unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                storage.as_mut_ptr().cast(),
                size_of_val(&storage) as u32,
                &mut returned,
            )
        })?;
        if (returned as usize) < size_of::<TOKEN_USER>() {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let user = unsafe { &*storage.as_ptr().cast::<TOKEN_USER>() };
        if self
            .allowed
            .iter()
            .any(|sid| unsafe { EqualSid(user.User.Sid, sid.0) != 0 })
        {
            Ok(())
        } else {
            Err(io::ErrorKind::PermissionDenied.into())
        }
    }
}

struct Impersonation;
impl Drop for Impersonation {
    fn drop(&mut self) {
        if unsafe { RevertToSelf() } == 0 {
            // Continuing under an untrusted identity would violate the boundary.
            std::process::abort();
        }
    }
}

fn pause_until(deadline: Instant) -> io::Result<()> {
    if STOP.load(Ordering::Acquire) {
        return Err(io::ErrorKind::ConnectionAborted.into());
    }
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(io::ErrorKind::TimedOut.into());
    }
    std::thread::sleep(left.min(POLL));
    Ok(())
}

impl Transport for Listener {
    fn read_bounded(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        let deadline = Instant::now() + timeout;
        loop {
            let mut read = 0;
            if unsafe {
                ReadFile(
                    self.pipe.0,
                    buf.as_mut_ptr(),
                    buf.len() as u32,
                    &mut read,
                    null_mut(),
                )
            } != 0
            {
                return Ok(read as usize);
            }
            let e = io::Error::last_os_error();
            if e.raw_os_error() != Some(ERROR_NO_DATA as i32) {
                return Err(e);
            }
            pause_until(deadline)?;
        }
    }

    fn write_bounded(&mut self, buf: &[u8], timeout: Duration) -> io::Result<usize> {
        let deadline = Instant::now() + timeout;
        loop {
            let mut written = 0;
            checked(unsafe {
                WriteFile(
                    self.pipe.0,
                    buf.as_ptr(),
                    buf.len() as u32,
                    &mut written,
                    null_mut(),
                )
            })?;
            if written != 0 || buf.is_empty() {
                return Ok(written as usize);
            }
            pause_until(deadline)?;
        }
    }

    fn authorize_request(&mut self) -> io::Result<()> {
        if STOP.load(Ordering::Acquire) {
            return Err(io::ErrorKind::ConnectionAborted.into());
        }
        self.user_allowed()
    }
}

fn serve(
    status: SERVICE_STATUS_HANDLE,
    config: &WindowsBrokerConfig,
    handle: Handler,
) -> io::Result<()> {
    report(status, SERVICE_START_PENDING, ERROR_SUCCESS)?;
    let mut listener = Listener::create(config)?;
    report(status, SERVICE_RUNNING, ERROR_SUCCESS)?;
    while listener.accept()? {
        if daemon_http::exchange(&mut listener, |route| handle(config, route)).is_ok() {
            // DisconnectNamedPipe discards unread reply bytes. Give the client
            // a bounded interval to consume the Content-Length and close.
            let mut extra = [0u8; 1];
            let _ = listener.read_bounded(&mut extra, Duration::from_secs(2));
        }
        listener.disconnect()?;
    }
    report(status, SERVICE_STOP_PENDING, ERROR_SUCCESS)
}
