use super::daemon_http::{Reply, Route};
use serde::Deserialize;
use std::io;

pub(super) const RESPONSE_LIMIT: usize = 512 * 1024;

pub(super) fn header_end(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(4)
        .take(8192)
        .position(|window| window == b"\r\n\r\n")
}

pub(super) fn content_length(head: &[u8]) -> io::Result<Option<usize>> {
    let text = std::str::from_utf8(head).map_err(|_| io::ErrorKind::InvalidData)?;
    let mut result = None;
    for line in text.split("\r\n").skip(1) {
        let (name, value) = line.split_once(':').ok_or(io::ErrorKind::InvalidData)?;
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(io::ErrorKind::InvalidData.into());
        }
        if name.eq_ignore_ascii_case("content-length") {
            let length = value
                .trim()
                .parse::<usize>()
                .map_err(|_| io::ErrorKind::InvalidData)?;
            if result.is_some() || length > RESPONSE_LIMIT {
                return Err(io::ErrorKind::InvalidData.into());
            }
            result = Some(length);
        }
    }
    Ok(result)
}

pub(super) fn project_info(bytes: &[u8], expected: &str, route: Route) -> io::Result<Reply> {
    let end = header_end(bytes).ok_or(io::ErrorKind::InvalidData)?;
    let head = &bytes[..end];
    if !(head.starts_with(b"HTTP/1.0 200 ") || head.starts_with(b"HTTP/1.1 200 ")) {
        return Err(io::ErrorKind::InvalidData.into());
    }
    #[derive(Deserialize)]
    struct Identity {
        #[serde(rename = "ID")]
        id: String,
    }
    let body = &bytes[end + 4..];
    let limited = serde_json::from_slice::<Identity>(body)?;
    if limited.id.is_empty() || limited.id.len() > 128 || !limited.id.is_ascii() {
        return Err(io::ErrorKind::InvalidData.into());
    }
    if limited.id != expected {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let projection = match route {
        Route::Ping => return Ok(Reply::ping()),
        Route::DockerInfo => serde_json::json!({"ID": limited.id}),
        Route::Inventory => inventory_summary(&limited.id, body)?,
    };
    Ok(Reply::json(serde_json::to_vec(&projection)?))
}

fn inventory_summary(id: &str, body: &[u8]) -> io::Result<serde_json::Value> {
    #[derive(Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct Counts {
        containers: Option<u64>,
        images: Option<u64>,
    }
    let counts: Counts = serde_json::from_slice(body)?;
    Ok(serde_json::json!({
        "engine_id": id,
        "status": "unknown",
        "complete": false,
        "container_count": counts.containers,
        "image_count": counts.images,
        "volume_count": null,
        "duplicate_groups": null,
        "containers": null,
        "images": null,
        "volumes": null,
        "duplicates": null,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn inventory_only_reports_counts_observed_on_the_pinned_engine() {
        let body = br#"{"ID":"engine","Containers":0,"Images":8,"Extra":"fixture"}"#;
        assert_eq!(
            inventory_summary("engine", body).unwrap(),
            json!({"engine_id":"engine","status":"unknown","complete":false,
                "container_count":0,"image_count":8,"volume_count":null,"duplicate_groups":null,
                "containers":null,"images":null,"volumes":null,"duplicates":null})
        );
        let response = [b"HTTP/1.0 200 OK\r\n\r\n".as_slice(), body].concat();
        assert!(project_info(&response, "engine", Route::Inventory).is_ok());
        assert!(project_info(&response, "other", Route::Inventory).is_err());
    }

    #[test]
    fn missing_counts_are_unknown_and_malformed_counts_fail_closed() {
        let summary = inventory_summary("engine", br#"{"Containers":2}"#).unwrap();
        assert_eq!(summary.get("image_count"), Some(&serde_json::Value::Null));
        for body in [
            br#"{"Images":-1}"#.as_slice(),
            br#"{"Containers":"3"}"#,
            br#"{"Images":1,"Images":2}"#,
        ] {
            assert!(inventory_summary("engine", body).is_err());
        }
    }

    #[test]
    fn rejects_ambiguous_or_unbounded_framing() {
        assert_eq!(
            content_length(b"HTTP/1.0 200 OK\r\nContent-Length: 27").unwrap(),
            Some(27)
        );
        for header in [
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked".as_slice(),
            b"HTTP/1.0 200 OK\r\nContent-Length: 1\r\ncontent-length: 1",
            b"HTTP/1.0 200 OK\r\nContent-Length: 524289",
        ] {
            assert!(content_length(header).is_err());
        }
    }
}
