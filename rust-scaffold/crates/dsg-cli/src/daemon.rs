use clap::Parser;
use std::{path::PathBuf, process::ExitCode};

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
mod daemon_http;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod daemon_socket;
#[cfg(windows)]
mod daemon_windows;
#[cfg(any(windows, test))]
mod daemon_windows_config;
#[cfg(windows)]
mod daemon_windows_upstream;

#[derive(Parser)]
#[command(name = "dsgd", about = "로컬 Docker 읽기 전용 브로커")]
struct Options {
    #[arg(long)]
    listen: PathBuf,
    #[arg(long)]
    upstream: PathBuf,
    #[arg(long)]
    engine_id: String,
    #[cfg(unix)]
    #[arg(long)]
    docker_executable: PathBuf,
    #[cfg(unix)]
    #[arg(long)]
    empty_config_dir: PathBuf,
    #[cfg(unix)]
    #[arg(long, required = true)]
    allow_uid: Vec<u32>,
    #[cfg(unix)]
    #[arg(long, default_value_t = 16, value_parser = clap::value_parser!(u32).range(1..=32))]
    max_resources: u32,
    #[cfg(windows)]
    #[arg(long)]
    service_name: String,
    #[cfg(windows)]
    #[arg(long)]
    allow_sid: String,
    #[cfg(windows)]
    #[arg(long)]
    upstream_sid: String,
}

fn main() -> ExitCode {
    let options = Options::parse();
    match run(&options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => {
            eprintln!("DSGD_UNAVAILABLE: 브로커 설정과 서비스 권한을 확인하세요");
            ExitCode::from(5)
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn run(_: &Options) -> std::io::Result<()> {
    Err(std::io::ErrorKind::Unsupported.into())
}

#[cfg(windows)]
fn run(options: &Options) -> std::io::Result<()> {
    use daemon_windows_config::{WindowsBrokerConfig, WindowsSid};
    use std::io;

    let config = WindowsBrokerConfig::parse(
        options.listen.to_str().ok_or(io::ErrorKind::InvalidInput)?,
        options
            .upstream
            .to_str()
            .ok_or(io::ErrorKind::InvalidInput)?,
        &options.service_name,
        &options.allow_sid,
    )
    .map_err(|_| io::ErrorKind::InvalidInput)?;
    let upstream_sid =
        WindowsSid::parse(&options.upstream_sid).map_err(|_| io::ErrorKind::InvalidInput)?;
    if config.client_sids().contains(&upstream_sid)
        || config
            .client_sids()
            .iter()
            .any(|sid| sid.as_str() == "S-1-5-18")
    {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    daemon_windows_upstream::configure(upstream_sid, &options.engine_id)?;
    daemon_windows::run(config, daemon_windows_upstream::observe)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn run(options: &Options) -> std::io::Result<()> {
    use std::io;

    if options.allow_uid.len() > 16 || options.engine_id.is_empty() || options.engine_id.len() > 128
    {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    validate_client(options)?;
    daemon_socket::serve(
        &options.listen,
        &options.upstream,
        &options.allow_uid,
        |mut connection, _, stopping| {
            daemon_http::serve(&mut connection, |route| {
                if stopping.load(std::sync::atomic::Ordering::Relaxed) {
                    return daemon_http::Reply::unavailable();
                }
                observe(options, route).unwrap_or_else(daemon_http::Reply::unavailable)
            })
        },
    )
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn validate_client(options: &Options) -> std::io::Result<()> {
    use std::{fs, io, os::unix::fs::MetadataExt};

    let owner = unsafe { libc::geteuid() };
    if options.docker_executable.canonicalize()? != options.docker_executable {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    daemon_socket::controlled_directory(
        options
            .docker_executable
            .parent()
            .ok_or(io::ErrorKind::InvalidInput)?,
    )?;
    let file = fs::symlink_metadata(&options.docker_executable)?;
    let config = daemon_socket::controlled_directory(&options.empty_config_dir)?;
    if !file.is_file()
        || (file.uid() != owner && file.uid() != 0)
        || file.mode() & 0o022 != 0
        || file.mode() & 0o111 == 0
        || config.uid() != owner
        || config.mode() & 0o077 != 0
        || fs::read_dir(&options.empty_config_dir)?.next().is_some()
    {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    daemon_socket::reject_extended_grants(&options.docker_executable)?;
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn observe(options: &Options, route: daemon_http::Route) -> Option<daemon_http::Reply> {
    use daemon_http::{Reply, Route};
    use dsg_core::{docker, docker_command::DockerCli};
    use serde_json::json;

    daemon_socket::validate_upstream(&options.listen, &options.upstream).ok()?;
    validate_client(options).ok()?;
    let client = DockerCli::for_local_socket(
        &options.docker_executable,
        &options.upstream,
        &options.empty_config_dir,
    )
    .ok()?;

    if docker::query_engine(&client).ok()? != options.engine_id {
        return None;
    }
    match route {
        Route::Ping => Some(Reply::ping()),
        Route::DockerInfo => Some(Reply::json(
            json!({"ID": options.engine_id}).to_string().into_bytes(),
        )),
        Route::Inventory => {
            let inventory = docker::inventory(&client, options.max_resources as usize);
            if !inventory.complete || inventory.engine_id.as_deref() != Some(&options.engine_id) {
                return None;
            }
            Some(Reply::json(serde_json::to_vec(&inventory).ok()?))
        }
    }
}
