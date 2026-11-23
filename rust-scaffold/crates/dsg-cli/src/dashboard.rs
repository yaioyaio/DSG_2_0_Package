use super::{filesystem, mcp, Cli, Policy};
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::process::ExitCode;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::{Duration, Instant};

const INDEX: &[u8] = include_bytes!("../dashboard/INDEX.html");
const STYLE: &[u8] = include_bytes!("../dashboard/styles.css");
const SCRIPT: &[u8] = include_bytes!("../dashboard/app.js");
const HTTP_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_REQUEST_BYTES: usize = 8192;

pub(super) fn run(cli: &Cli, port: u16, lifetime: u16, docker_context: Option<&str>) -> ExitCode {
    match start(cli, port, lifetime, docker_context) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => {
            eprintln!("DASHBOARD_UNAVAILABLE: 프로젝트 정책과 로컬 포트를 확인하세요");
            ExitCode::from(2)
        }
    }
}

fn start(cli: &Cli, port: u16, lifetime: u16, docker_context: Option<&str>) -> io::Result<()> {
    let root = filesystem::canonical_root(&cli.root).map_err(io::Error::other)?;
    let policy = Policy::load(
        &root,
        cli.global_policy.as_deref(),
        cli.session_policy.as_deref(),
    )
    .map_err(io::Error::other)?;
    let server = TcpListener::bind((Ipv4Addr::LOCALHOST, port))?;
    server.set_nonblocking(true)?;
    let host = server.local_addr()?.to_string();
    let url = format!("http://{host}");
    let stopping = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&stopping);
    ctrlc::set_handler(move || signal.store(true, Ordering::Release)).map_err(io::Error::other)?;
    if cli.json {
        println!(
            "{}",
            serde_json::json!({"schema_version":1,"command":"dashboard","result":{"url":url}})
        );
    } else {
        println!("{url}");
    }
    io::stdout().flush()?;
    let death = Instant::now() + Duration::from_secs(u64::from(lifetime));
    let mut cache: Option<(Instant, Vec<u8>)> = None;
    while Instant::now() < death && !stopping.load(Ordering::Acquire) {
        match server.accept() {
            Ok((mut stream, endpoint)) if endpoint.ip().is_loopback() => {
                // The accepted socket can inherit nonblocking mode on Windows.
                // The request handler uses bounded, blocking reads and writes.
                if stream.set_nonblocking(false).is_err() {
                    continue;
                }
                let _ = handle(&mut stream, &host, &url, || {
                    let stale = cache
                        .as_ref()
                        .is_none_or(|(when, _)| when.elapsed() >= Duration::from_secs(2));
                    if stale {
                        let bytes = mcp::overview(&root, &policy, docker_context)
                            .to_string()
                            .into_bytes();
                        cache = Some((Instant::now(), bytes));
                    }
                    cache
                        .as_ref()
                        .map(|(_, bytes)| bytes.clone())
                        .unwrap_or_default()
                });
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(25))
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[derive(Debug, Eq, PartialEq)]
enum Route {
    Index,
    Style,
    Script,
    Overview,
}

fn parse(header: &[u8], host: &str, origin: &str) -> Result<Route, &'static str> {
    let request = std::str::from_utf8(header).map_err(|_| "400 Bad Request")?;
    let mut lines = request.split("\r\n");
    let mut first = lines.next().unwrap_or_default().split(' ');
    let (Some(method), Some(path), Some("HTTP/1.1"), None) =
        (first.next(), first.next(), first.next(), first.next())
    else {
        return Err("400 Bad Request");
    };
    if method != "GET" {
        return Err("405 Method Not Allowed");
    }
    let mut seen = std::collections::BTreeSet::new();
    for line in lines {
        let (name, value) = line.split_once(':').ok_or("400 Bad Request")?;
        let name = name.to_ascii_lowercase();
        let value = value.trim_matches([' ', '\t']);
        if name.is_empty()
            || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || !value
                .bytes()
                .all(|b| b == b'\t' || (0x20..=0x7e).contains(&b))
            || !seen.insert(name.clone())
        {
            return Err("400 Bad Request");
        }
        match name.as_str() {
            "host" if value != host => return Err("403 Forbidden"),
            "origin" if value != origin => return Err("403 Forbidden"),
            "sec-fetch-site" if !matches!(value, "none" | "same-origin") => {
                return Err("403 Forbidden")
            }
            "transfer-encoding" | "upgrade" => return Err("400 Bad Request"),
            "content-length" if value != "0" => return Err("400 Bad Request"),
            _ => {}
        }
    }
    if !seen.contains("host") {
        return Err("400 Bad Request");
    }
    match path {
        "/" => Ok(Route::Index),
        "/styles.css" => Ok(Route::Style),
        "/app.js" => Ok(Route::Script),
        "/api/overview" => Ok(Route::Overview),
        _ => Err("404 Not Found"),
    }
}

fn time_left(deadline: Instant) -> io::Result<Duration> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        Err(io::ErrorKind::TimedOut.into())
    } else {
        Ok(remaining)
    }
}

fn header(stream: &mut TcpStream) -> io::Result<Option<Vec<u8>>> {
    let deadline = Instant::now() + HTTP_TIMEOUT;
    let mut bytes = [0_u8; MAX_REQUEST_BYTES];
    let mut used: usize = 0;
    while used < bytes.len() {
        stream.set_read_timeout(Some(time_left(deadline)?))?;
        let count = stream.read(&mut bytes[used..])?;
        if count == 0 {
            break;
        }
        let start = used.saturating_sub(3);
        used += count;
        if let Some(index) = bytes[start..used]
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
        {
            let end = start + index;
            return Ok((end + 4 == used).then(|| bytes[..end].to_vec()));
        }
    }
    Ok(None)
}

fn handle(
    stream: &mut TcpStream,
    host: &str,
    origin: &str,
    overview: impl FnOnce() -> Vec<u8>,
) -> io::Result<()> {
    let route = header(stream)?
        .as_deref()
        .ok_or("400 Bad Request")
        .and_then(|head| parse(head, host, origin));
    let generated;
    let (status, content_type, body) = match route {
        Ok(Route::Index) => ("200 OK", "text/html; charset=utf-8", INDEX),
        Ok(Route::Style) => ("200 OK", "text/css; charset=utf-8", STYLE),
        Ok(Route::Script) => ("200 OK", "text/javascript; charset=utf-8", SCRIPT),
        Ok(Route::Overview) => {
            generated = overview();
            ("200 OK", "application/json", generated.as_slice())
        }
        Err(status) => (status, "text/plain; charset=utf-8", status.as_bytes()),
    };
    let headers = format!(concat!("HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n",
        "Cache-Control: no-store\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\n",
        "Referrer-Policy: no-referrer\r\nCross-Origin-Resource-Policy: same-origin\r\n",
        "Content-Security-Policy: default-src 'none'; style-src 'self'; script-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'\r\n\r\n"),
        status, content_type, body.len());
    let deadline = Instant::now() + HTTP_TIMEOUT;
    for mut bytes in [headers.as_bytes(), body] {
        while !bytes.is_empty() {
            stream.set_write_timeout(Some(time_left(deadline)?))?;
            let written = stream.write(bytes)?;
            if written == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            bytes = &bytes[written..];
        }
    }
    Ok(())
}
