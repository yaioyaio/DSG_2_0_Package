use super::daemon_engine_info::{content_length, header_end, project_info, RESPONSE_LIMIT};
use super::daemon_http::{Reply, Route};
use super::daemon_windows_config::{WindowsBrokerConfig, WindowsSid};
use std::io;
use std::ptr::{null, null_mut};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    CloseHandle, LocalFree, ERROR_BROKEN_PIPE, ERROR_NO_DATA, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSidToSidW, GetSecurityInfo, SE_KERNEL_OBJECT,
};
use windows_sys::Win32::Security::{
    EqualSid, GetAce, GetTokenInformation, TokenUser, ACCESS_ALLOWED_ACE, ACE_HEADER,
    DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PSID, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_READ_DATA, FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA,
    OPEN_EXISTING, READ_CONTROL, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT, SYNCHRONIZE,
};
use windows_sys::Win32::System::Pipes::{
    GetNamedPipeServerProcessId, SetNamedPipeHandleState, PIPE_NOWAIT,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

static EXPECTED: OnceLock<(WindowsSid, String)> = OnceLock::new();

pub(super) fn configure(sid: WindowsSid, engine: &str) -> io::Result<()> {
    if engine.is_empty() || engine.len() > 128 || !engine.is_ascii() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    EXPECTED
        .set((sid, engine.to_owned()))
        .map_err(|_| io::ErrorKind::AlreadyExists.into())
}

pub(super) fn observe(config: &WindowsBrokerConfig, route: Route) -> Reply {
    let Some((sid, engine)) = EXPECTED.get() else {
        return Reply::unavailable();
    };
    read_info(config.upstream(), sid)
        .and_then(|response| project_info(&response, engine, route))
        .unwrap_or_else(|_| Reply::unavailable())
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

struct LocalAllocation(*mut std::ffi::c_void);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        unsafe { LocalFree(self.0) };
    }
}

fn numeric_sid(value: &str) -> io::Result<LocalAllocation> {
    let wide: Vec<_> = value.encode_utf16().chain([0]).collect();
    let mut sid = null_mut();
    if unsafe { ConvertStringSidToSidW(wide.as_ptr(), &mut sid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(LocalAllocation(sid))
}

fn token_sid(process: HANDLE) -> io::Result<TokenSid> {
    let mut token = null_mut();
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = OwnedHandle(token);
    // An aligned buffer large enough for TOKEN_USER and the maximum binary SID.
    let mut words = Box::new([0usize; 32]);
    let mut length = 0;
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            words.as_mut_ptr().cast(),
            std::mem::size_of_val(&*words) as u32,
            &mut length,
        )
    } == 0
        || (length as usize) < std::mem::size_of::<TOKEN_USER>()
    {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(TokenSid(words))
}

struct TokenSid(Box<[usize; 32]>);

impl TokenSid {
    fn as_sid(&self) -> PSID {
        unsafe { (*self.0.as_ptr().cast::<TOKEN_USER>()).User.Sid }
    }
}

fn verify_pipe(pipe: HANDLE, expected: PSID) -> io::Result<()> {
    let mut pid = 0;
    if unsafe { GetNamedPipeServerProcessId(pipe, &mut pid) } == 0 || pid == 0 {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return Err(io::Error::last_os_error());
    }
    let process = OwnedHandle(process);
    let server = token_sid(process.0)?;
    let current = token_sid(unsafe { GetCurrentProcess() })?;
    let system = numeric_sid("S-1-5-18")?;
    let admins = numeric_sid("S-1-5-32-544")?;
    let mut owner = null_mut();
    let mut dacl = null_mut();
    let mut descriptor = null_mut();
    let result = unsafe {
        GetSecurityInfo(
            pipe,
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut descriptor,
        )
    };
    let _descriptor = LocalAllocation(descriptor);
    if result != 0
        || owner.is_null()
        || dacl.is_null()
        || unsafe { (*dacl).AceCount } > 32
        || unsafe { EqualSid(owner, expected) } == 0
        || unsafe { EqualSid(server.as_sid(), expected) } == 0
    {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    for i in 0..unsafe { (*dacl).AceCount as u32 } {
        let mut ace = null_mut();
        if unsafe { GetAce(dacl, i, &mut ace) } == 0 || ace.is_null() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        match unsafe { (*ace.cast::<ACE_HEADER>()).AceType } {
            1 => continue, // ACCESS_DENIED_ACE_TYPE cannot broaden access.
            0 => {
                let ace = ace.cast::<ACCESS_ALLOWED_ACE>();
                let sid = unsafe { std::ptr::addr_of!((*ace).SidStart).cast_mut().cast() };
                let trusted = [expected, current.as_sid(), system.0, admins.0];
                if trusted
                    .into_iter()
                    .any(|entry| unsafe { EqualSid(entry, sid) } != 0)
                {
                    continue;
                }
            }
            _ => {}
        }
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(())
}

fn read_info(path: &str, sid: &WindowsSid) -> io::Result<Vec<u8>> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let path: Vec<_> = path.encode_utf16().chain([0]).collect();
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            FILE_READ_DATA | FILE_WRITE_DATA | FILE_WRITE_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE,
            0,
            null(),
            OPEN_EXISTING,
            SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let handle = OwnedHandle(handle);
    let sid = numeric_sid(sid.as_str())?;
    verify_pipe(handle.0, sid.0)?;
    let mode = PIPE_NOWAIT;
    if unsafe { SetNamedPipeHandleState(handle.0, &mode, null(), null()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let request = b"GET /info HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n";
    if Instant::now() >= deadline {
        return Err(io::ErrorKind::TimedOut.into());
    }
    let mut count = 0;
    if unsafe {
        WriteFile(
            handle.0,
            request.as_ptr(),
            request.len() as u32,
            &mut count,
            null_mut(),
        )
    } == 0
        || count != request.len() as u32
    {
        return Err(io::ErrorKind::BrokenPipe.into());
    }
    let mut response = Vec::new();
    let mut framed_end = None;
    let mut header_read = false;
    let mut buffer = [0u8; 4096];
    loop {
        if Instant::now() >= deadline || response.len() > RESPONSE_LIMIT {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let available = unsafe {
            ReadFile(
                handle.0,
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                &mut count,
                null_mut(),
            )
        };
        if available != 0 && count > 0 {
            response.extend_from_slice(&buffer[..count as usize]);
            if response.len() > RESPONSE_LIMIT {
                return Err(io::ErrorKind::InvalidData.into());
            }
            if !header_read {
                if let Some(end) = header_end(&response) {
                    header_read = true;
                    framed_end = content_length(&response[..end])?.map(|length| end + 4 + length);
                } else if response.len() >= 8196 {
                    return Err(io::ErrorKind::InvalidData.into());
                }
            }
            if let Some(end) = framed_end {
                match response.len().cmp(&end) {
                    std::cmp::Ordering::Greater => return Err(io::ErrorKind::InvalidData.into()),
                    std::cmp::Ordering::Equal => break,
                    std::cmp::Ordering::Less => {}
                }
            }
            continue;
        }
        if available != 0 {
            break;
        }
        match io::Error::last_os_error()
            .raw_os_error()
            .map(|code| code as u32)
        {
            Some(ERROR_NO_DATA) => thread::sleep(Duration::from_millis(5)),
            Some(ERROR_BROKEN_PIPE) => break,
            _ => return Err(io::ErrorKind::BrokenPipe.into()),
        }
    }
    if framed_end.is_some_and(|end| end != response.len()) {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    Ok(response)
}
