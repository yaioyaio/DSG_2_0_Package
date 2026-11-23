use std::io;
#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::net::Shutdown;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

const MAX_HEADERS_BYTES: usize = 8192;
const MAX_BODY_BYTES: usize = 1024 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Route {
    Ping,
    DockerInfo,
    Inventory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ErrorStatus {
    BadRequest,
    Forbidden,
    Timeout,
    TooLarge,
    Unavailable,
}

impl ErrorStatus {
    fn reason(self) -> &'static str {
        match self {
            Self::BadRequest => "400 Bad Request",
            Self::Forbidden => "403 Forbidden",
            Self::Timeout => "408 Request Timeout",
            Self::TooLarge => "431 Request Header Fields Too Large",
            Self::Unavailable => "503 Service Unavailable",
        }
    }
}

pub(crate) struct Reply {
    status: Option<ErrorStatus>,
    content_type: &'static str,
    body: Vec<u8>,
}

impl Reply {
    pub(crate) fn ping() -> Self {
        Self {
            status: None,
            content_type: "text/plain",
            body: b"OK".to_vec(),
        }
    }

    pub(crate) fn json(body: Vec<u8>) -> Self {
        if body.len() > MAX_BODY_BYTES {
            return Self::unavailable();
        }
        Self {
            status: None,
            content_type: "application/json",
            body,
        }
    }

    pub(crate) fn unavailable() -> Self {
        Self::error(ErrorStatus::Unavailable)
    }

    fn error(status: ErrorStatus) -> Self {
        Self {
            status: Some(status),
            content_type: "text/plain",
            body: status.reason().as_bytes().to_vec(),
        }
    }
}

fn route(path: &str) -> Result<Route, ErrorStatus> {
    match path {
        "/_ping" => Ok(Route::Ping),
        "/info" => Ok(Route::DockerInfo),
        "/dsg/v1/inventory" => Ok(Route::Inventory),
        _ => {
            let version = path
                .strip_prefix("/v1.")
                .and_then(|rest| rest.strip_suffix("/info"));
            if version.is_some_and(|s| {
                (1..=3).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit())
            }) {
                Ok(Route::DockerInfo)
            } else {
                Err(ErrorStatus::Forbidden)
            }
        }
    }
}

// Only fields used by local read-only clients are accepted. No field reaches the backend.
fn parse_request(head: &[u8]) -> Result<Route, ErrorStatus> {
    let head = std::str::from_utf8(head).map_err(|_| ErrorStatus::BadRequest)?;
    let mut lines = head.split("\r\n");
    let mut request = lines.next().unwrap_or_default().split(' ');
    let (Some(method), Some(path), Some(version), None) = (
        request.next(),
        request.next(),
        request.next(),
        request.next(),
    ) else {
        return Err(ErrorStatus::BadRequest);
    };
    if !matches!(version, "HTTP/1.0" | "HTTP/1.1") {
        return Err(ErrorStatus::BadRequest);
    }
    if method != "GET" {
        return Err(ErrorStatus::Forbidden);
    }
    let target = route(path)?;
    let mut seen = Vec::new();
    for line in lines {
        let (name, raw) = line.split_once(':').ok_or(ErrorStatus::BadRequest)?;
        let value = raw.trim_matches(' ');
        if value.is_empty() || !raw.bytes().all(|b| (32..=126).contains(&b)) {
            return Err(ErrorStatus::BadRequest);
        }
        let name = name.to_ascii_lowercase();
        if seen.contains(&name) {
            return Err(ErrorStatus::BadRequest);
        }
        match name.as_str() {
            "host"
                if value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b".-:[]_".contains(&b)) => {}
            "accept" | "accept-encoding" | "user-agent" => {}
            "content-length" if value == "0" => {}
            "connection"
                if value.eq_ignore_ascii_case("close")
                    || value.eq_ignore_ascii_case("keep-alive") => {}
            _ => return Err(ErrorStatus::BadRequest),
        }
        seen.push(name);
    }
    if version == "HTTP/1.1" && !seen.iter().any(|name| name == "host") {
        return Err(ErrorStatus::BadRequest);
    }
    Ok(target)
}

fn remaining(deadline: Instant) -> io::Result<Duration> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(io::ErrorKind::TimedOut.into());
    }
    Ok(left)
}

pub(crate) trait Transport {
    fn read_bounded(&mut self, buffer: &mut [u8], timeout: Duration) -> io::Result<usize>;
    fn write_bounded(&mut self, buffer: &[u8], timeout: Duration) -> io::Result<usize>;
    fn authorize_request(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(unix)]
impl Transport for UnixStream {
    fn read_bounded(&mut self, buffer: &mut [u8], timeout: Duration) -> io::Result<usize> {
        self.set_read_timeout(Some(timeout))?;
        self.read(buffer)
    }

    fn write_bounded(&mut self, buffer: &[u8], timeout: Duration) -> io::Result<usize> {
        self.set_write_timeout(Some(timeout))?;
        self.write(buffer)
    }
}

fn read_route(stream: &mut impl Transport) -> io::Result<Result<Route, ErrorStatus>> {
    let deadline = Instant::now() + IO_TIMEOUT;
    let mut header = [0; MAX_HEADERS_BYTES];
    let mut used: usize = 0;
    loop {
        let n = match stream.read_bounded(&mut header[used..], remaining(deadline)?) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            other => other?,
        };
        if n == 0 {
            return Ok(Err(ErrorStatus::BadRequest));
        }
        let start = used.saturating_sub(3);
        used += n;
        if let Some(i) = header[start..used]
            .windows(4)
            .position(|s| s == b"\r\n\r\n")
        {
            let end = start + i;
            if end + 4 != used {
                return Ok(Err(ErrorStatus::BadRequest));
            }
            return Ok(parse_request(&header[..end]));
        }
        if used == header.len() {
            return Ok(Err(ErrorStatus::TooLarge));
        }
    }
}

fn write_reply(stream: &mut impl Transport, reply: Reply) -> io::Result<()> {
    let deadline = Instant::now() + IO_TIMEOUT;
    let status = reply.status.map(ErrorStatus::reason).unwrap_or("200 OK");
    let headers = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        reply.content_type,
        reply.body.len()
    );
    for mut bytes in [headers.as_bytes(), reply.body.as_slice()] {
        while !bytes.is_empty() {
            match stream.write_bounded(bytes, remaining(deadline)?) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) => bytes = &bytes[n..],
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
    }
    Ok(())
}

pub(crate) fn exchange(
    stream: &mut impl Transport,
    response: impl FnOnce(Route) -> Reply,
) -> io::Result<()> {
    let reply = match read_route(stream) {
        Ok(Ok(route)) => {
            stream.authorize_request()?;
            response(route)
        }
        Ok(Err(status)) => Reply::error(status),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            ) =>
        {
            Reply::error(ErrorStatus::Timeout)
        }
        Err(e) => return Err(e),
    };
    write_reply(stream, reply)
}

#[cfg(unix)]
pub(crate) fn serve(
    stream: &mut UnixStream,
    response: impl FnOnce(Route) -> Reply,
) -> io::Result<()> {
    let written = exchange(stream, response);
    let closed = stream.shutdown(Shutdown::Both);
    written.and(closed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_read_routes() {
        for (path, expected) in [
            ("/_ping", Route::Ping),
            ("/info", Route::DockerInfo),
            ("/v1.9/info", Route::DockerInfo),
            ("/v1.123/info", Route::DockerInfo),
            ("/dsg/v1/inventory", Route::Inventory),
        ] {
            let head = format!("GET {path} HTTP/1.1\r\nHost: docker\r\nContent-Length: 0");
            assert_eq!(parse_request(head.as_bytes()), Ok(expected));
        }
        assert_eq!(parse_request(b"GET /info HTTP/1.0"), Ok(Route::DockerInfo));
        for path in ["/v1./info", "/v1.1234/info", "/info?all", "/unknown"] {
            assert_eq!(route(path), Err(ErrorStatus::Forbidden));
        }
    }

    #[test]
    fn rejects_unsupported_request_shapes() {
        for head in [
            "POST /info HTTP/1.0",
            "GET /info HTTP/1.1",
            "GET /info HTTP/1.0\r\nContent-Length: 1",
            "GET /info HTTP/1.0\r\nTransfer-Encoding: chunked",
            "GET /info HTTP/1.0\r\nConnection: upgrade",
            "GET /info HTTP/1.0\r\nUpgrade: websocket",
            "GET /info HTTP/1.1\r\nHost: local\r\nhost: local",
            "GET /info HTTP/1.0\r\n Accept: x",
        ] {
            assert!(parse_request(head.as_bytes()).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn invalid_input_never_dispatches() {
        for input in [
            b"GET /info HTTP/1.0\r\n\r\nextra".as_slice(),
            &[b'x'; MAX_HEADERS_BYTES],
        ] {
            let (mut client, mut server) = UnixStream::pair().unwrap();
            client.write_all(input).unwrap();
            serve(&mut server, |_| panic!("invalid request dispatched")).unwrap();
            let mut reply = String::new();
            client.read_to_string(&mut reply).unwrap();
            assert!(reply.starts_with("HTTP/1.1 4"));
        }
    }
}
