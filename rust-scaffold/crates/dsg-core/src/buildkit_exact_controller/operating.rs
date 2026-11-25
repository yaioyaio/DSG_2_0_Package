//! A single host-side listener enters only the keeper named by the deployment descriptor.
use super::{bounded_read, check_alive, io_failure, protected_file, readable, stage, Result};
use crate::docker::QueryFailure;
use crate::docker_control_local::{
    admission::write_request::enable_credentials, open_protected, process_start,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fs::{self, File, Permissions};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::{
    fs::{MetadataExt, PermissionsExt},
    process::CommandExt,
};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const DIRECTORY: &str = "/run/dsg-buildkit-exact";
const SOCKET: &str = "/run/dsg-buildkit-exact/control.sock";
const DESCRIPTOR: &str = "/etc/dsg-buildkit-exact/operating.json";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Operating {
    version: u8,
    registration: String,
    keeper: Keeper,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Keeper {
    pid: i32,
    start_ticks: u64,
    boot_sha256: String,
    mount_namespace: u64,
    network_namespace: u64,
}

struct Pinned {
    process: File,
    mount: File,
    network: File,
}

fn descriptor(fd: i32) -> Result<File> {
    if fd < 0 {
        return Err(io_failure(io::Error::last_os_error()));
    }
    // All callers transfer ownership of a fresh kernel descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

impl Keeper {
    fn pin(&self) -> Result<Pinned> {
        let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id").map_err(io_failure)?;
        if self.pid <= 1
            || self.start_ticks == 0
            || self.boot_sha256 != format!("{:x}", Sha256::digest(boot.trim().as_bytes()))
        {
            return Err(QueryFailure::InvalidSelector);
        }
        let process =
            descriptor(unsafe { libc::syscall(libc::SYS_pidfd_open, self.pid, 0) } as i32)?;
        let namespace = |name: &str, expected| {
            let pinned = File::open(format!("/proc/{}/ns/{name}", self.pid)).map_err(io_failure)?;
            let inode = pinned.metadata().map_err(io_failure)?.ino();
            let init = fs::metadata(format!("/proc/1/ns/{name}")).map_err(io_failure)?;
            if inode != expected || inode == init.ino() {
                return Err(QueryFailure::InvalidSelector);
            }
            Ok(pinned)
        };
        let mount = namespace("mnt", self.mount_namespace)?;
        let network = namespace("net", self.network_namespace)?;
        if process_start(self.pid)? != self.start_ticks {
            return Err(QueryFailure::InvalidSelector);
        }
        check_alive(&process)?;
        Ok(Pinned {
            process,
            mount,
            network,
        })
    }
}

fn listener() -> Result<File> {
    let dir = open_protected(Path::new(DIRECTORY), false)?
        .metadata()
        .map_err(io_failure)?;
    if !dir.is_dir()
        || dir.uid() != 0
        || dir.gid() == 0
        || dir.gid() != unsafe { libc::getegid() }
        || dir.mode() & 0o7777 != 0o750
    {
        return Err(QueryFailure::InvalidSelector);
    }
    let socket = descriptor(unsafe {
        libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0)
    })?;
    let fd = socket.as_raw_fd();
    enable_credentials(fd)?;
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_family = libc::AF_UNIX as _;
    for (slot, byte) in address.sun_path.iter_mut().zip(SOCKET.bytes()) {
        *slot = byte as _;
    }
    if unsafe {
        libc::bind(
            fd,
            (&address as *const libc::sockaddr_un).cast(),
            std::mem::size_of_val(&address) as _,
        )
    } != 0
    {
        return Err(io_failure(io::Error::last_os_error()));
    }
    fs::set_permissions(SOCKET, Permissions::from_mode(0o660)).map_err(io_failure)?;
    if unsafe { libc::listen(fd, 4) } != 0 {
        return Err(io_failure(io::Error::last_os_error()));
    }
    Ok(socket)
}

fn accept_live(socket: &File, keeper: &File) -> Result<File> {
    let mut events = [socket, keeper].map(|file| libc::pollfd {
        fd: file.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    });
    loop {
        if unsafe { libc::poll(events.as_mut_ptr(), 2, -1) } < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(io_failure(error));
        }
        // An expired keeper also wakes the idle service, before any later client
        // can mistake the old listener for an available registration.
        if events[1].revents != 0 || events[0].revents != libc::POLLIN {
            return Err(QueryFailure::Unavailable);
        }
        return descriptor(unsafe {
            libc::accept4(
                socket.as_raw_fd(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                libc::SOCK_CLOEXEC,
            )
        });
    }
}

fn run() -> Result<()> {
    if unsafe { libc::geteuid() } != 0 || unsafe { libc::getuid() } != 0 {
        return Err(QueryFailure::InvalidSelector);
    }
    let file = protected_file(Path::new(DESCRIPTOR), 0o400)?;
    let data = bounded_read(readable(&file)?, 4096)?;
    let operating: Operating =
        serde_json::from_slice(&data).map_err(|_| QueryFailure::Malformed)?;
    if operating.version != 1 {
        return Err(QueryFailure::InvalidSelector);
    }
    stage(&operating.registration)?;
    let keeper = operating.keeper.pin()?;
    let socket = listener()?;
    loop {
        let accepted = accept_live(&socket, &keeper.process)?;
        check_alive(&keeper.process)?;
        let (mount, network) = (keeper.mount.as_raw_fd(), keeper.network.as_raw_fd());
        let mut cmd = Command::new("/proc/self/exe");
        cmd.args(["buildkit-exact-controller", &operating.registration])
            .env_clear()
            .current_dir("/")
            .stdin(accepted)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // Only the child changes namespaces. Inode-pinned namespace handles cannot
        // retarget a reused PID. The native operator rechecks the live keeper and custody.
        unsafe {
            cmd.pre_exec(move || {
                if libc::setns(network, libc::CLONE_NEWNET) < 0
                    || libc::setns(mount, libc::CLONE_NEWNS) < 0
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = cmd.spawn().map_err(io_failure)?;
        let deadline = Instant::now() + Duration::from_secs(65);
        while child.try_wait().map_err(io_failure)?.is_none() {
            if Instant::now() >= deadline {
                child.kill().map_err(io_failure)?;
                child.wait().map_err(io_failure)?;
                // Recovery must inspect an interrupted native operation before resuming.
                return Err(QueryFailure::Timeout);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

pub fn serve_operating() -> u8 {
    let _ = run();
    5
}
