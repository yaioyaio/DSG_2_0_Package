//! Syntax validation for Windows broker configuration. The transport must
//! authenticate Windows tokens and verify pipe ownership and access separately.

const PIPE_PREFIX: &str = r"\\.\pipe\";
const MAX_PIPE_NAME_BYTES: usize = 256;
const MAX_SERVICE_NAME_BYTES: usize = 256;
const MAX_CLIENT_SIDS: usize = 32;
const MAX_SUB_AUTHORITIES: usize = 15;
const MAX_SID_BYTES: usize = "S-1-0x".len() + 12 + MAX_SUB_AUTHORITIES * 11;

pub(super) struct WindowsBrokerConfig {
    listener: LocalPipeName,
    upstream: LocalPipeName,
    service_name: ServiceName,
    client_sids: Vec<WindowsSid>,
}

impl WindowsBrokerConfig {
    pub(super) fn parse(
        listener: &str,
        upstream: &str,
        service_name: &str,
        client_sids: &str,
    ) -> Result<Self, &'static str> {
        let listener = LocalPipeName::parse(listener).ok_or("invalid listener pipe name")?;
        let upstream = LocalPipeName::parse(upstream).ok_or("invalid upstream pipe name")?;
        if listener == upstream {
            return Err("listener and upstream pipe names must differ");
        }
        let service_name = ServiceName::parse(service_name).ok_or("invalid SCM service name")?;
        if client_sids.len() > MAX_CLIENT_SIDS * (MAX_SID_BYTES + 1) - 1 {
            return Err("client SID list is too long");
        }
        let mut parsed_sids = Vec::new();
        for value in client_sids.split(',') {
            if parsed_sids.len() == MAX_CLIENT_SIDS {
                return Err("too many client SIDs");
            }
            let sid = WindowsSid::parse(value)?;
            if parsed_sids.contains(&sid) {
                return Err("duplicate client SID");
            }
            parsed_sids.push(sid);
        }
        Ok(Self {
            listener,
            upstream,
            service_name,
            client_sids: parsed_sids,
        })
    }

    pub(super) fn listener(&self) -> &str {
        &self.listener.0
    }

    pub(super) fn upstream(&self) -> &str {
        &self.upstream.0
    }

    pub(super) fn service_name(&self) -> &str {
        &self.service_name.0
    }

    pub(super) fn client_sids(&self) -> &[WindowsSid] {
        &self.client_sids
    }
}

#[derive(PartialEq, Eq)]
struct LocalPipeName(String);

impl LocalPipeName {
    fn parse(value: &str) -> Option<Self> {
        if value.len() > MAX_PIPE_NAME_BYTES {
            return None;
        }
        let prefix = value.get(..PIPE_PREFIX.len())?;
        let name = value.get(PIPE_PREFIX.len()..)?;
        if !prefix.eq_ignore_ascii_case(PIPE_PREFIX) || !valid_name(name) {
            return None;
        }
        Some(Self(format!("{PIPE_PREFIX}{}", name.to_ascii_lowercase())))
    }
}

struct ServiceName(String);

impl ServiceName {
    fn parse(value: &str) -> Option<Self> {
        if value.len() > MAX_SERVICE_NAME_BYTES || !valid_name(value) {
            return None;
        }
        Some(Self(value.to_ascii_lowercase()))
    }
}

fn valid_name(name: &str) -> bool {
    name.as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphanumeric)
        && !name.ends_with('.')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

/// A canonical numeric SID string. Windows token verification must use the
/// parsed SID value and compare it with the authenticated token SID.
#[derive(PartialEq, Eq)]
pub(super) struct WindowsSid(String);

impl WindowsSid {
    pub(super) fn parse(value: &str) -> Result<Self, &'static str> {
        if value.len() > MAX_SID_BYTES {
            return Err("invalid Windows SID");
        }
        let mut fields = value.split('-');
        if !fields
            .next()
            .is_some_and(|kind| kind.eq_ignore_ascii_case("S"))
            || fields.next() != Some("1")
        {
            return Err("invalid Windows SID");
        }
        let authority = fields.next().ok_or("invalid SID identifier authority")?;
        let mut canonical = String::from("S-1-");
        if let Some(hex) = authority
            .strip_prefix("0x")
            .or_else(|| authority.strip_prefix("0X"))
        {
            if hex.len() != 12 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err("invalid SID identifier authority");
            }
            let number =
                u64::from_str_radix(hex, 16).map_err(|_| "invalid SID identifier authority")?;
            if number <= u64::from(u32::MAX) {
                return Err("invalid SID identifier authority");
            }
            canonical.push_str("0x");
            canonical.push_str(&hex.to_ascii_uppercase());
        } else {
            parse_sid_decimal(authority).ok_or("invalid SID identifier authority")?;
            canonical.push_str(authority);
        }
        let mut sub_authorities = fields.peekable();
        if sub_authorities.peek().is_none() {
            return Err("Windows SID requires a sub-authority");
        }
        for (index, field) in sub_authorities.enumerate() {
            if index == MAX_SUB_AUTHORITIES || parse_sid_decimal(field).is_none() {
                return Err("invalid SID sub-authority");
            }
            canonical.push('-');
            canonical.push_str(field);
        }
        Ok(Self(canonical))
    }

    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

fn parse_sid_decimal(field: &str) -> Option<u32> {
    if field.is_empty()
        || field.len() > 10
        || (field.starts_with('0') && field.len() > 1)
        || !field.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    field.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LISTENER: &str = r"\\.\pipe\dsg-fixture";
    const UPSTREAM: &str = r"\\.\pipe\engine-fixture";
    const SERVICE: &str = "dsg-fixture";
    const SID: &str = "S-1-999-42";

    #[test]
    fn canonicalizes_names_and_sids() {
        let config = WindowsBrokerConfig::parse(
            r"\\.\PIPE\DSG-Fixture",
            UPSTREAM,
            "DSG.Fixture-2_0",
            "s-1-999-42,S-1-0X0001000000ab-4294967295",
        )
        .unwrap();
        assert_eq!(config.listener(), LISTENER);
        assert_eq!(config.upstream(), UPSTREAM);
        assert_eq!(config.service_name(), "dsg.fixture-2_0");
        assert_eq!(config.client_sids()[0].as_str(), SID);
        assert_eq!(
            config.client_sids()[1].as_str(),
            "S-1-0x0001000000AB-4294967295"
        );
    }

    #[test]
    fn rejects_nonlocal_nested_or_colliding_pipes() {
        for pipe in [
            r"\\host\pipe\dsg",
            r"\\?\pipe\dsg",
            "npipe:////./pipe/dsg",
            r"\\.\pipe\",
            r"\\.\pipe\name\other",
            r"\\.\pipe\name/other",
            r"\\.\pipe\name.",
            r"\\.\pipe\이름",
            r"\\.\PIPE\ENGINE-FIXTURE",
        ] {
            assert!(WindowsBrokerConfig::parse(pipe, UPSTREAM, SERVICE, SID).is_err());
        }
        assert!(LocalPipeName::parse(&format!("{PIPE_PREFIX}{}", "a".repeat(247))).is_some());
        assert!(LocalPipeName::parse(&format!("{PIPE_PREFIX}{}", "a".repeat(248))).is_none());
    }

    #[test]
    fn rejects_invalid_service_names() {
        for name in ["", " dsg", "dsg ", "a/b", r"a\b", "a\0b", "a.", "서비스"] {
            assert!(WindowsBrokerConfig::parse(LISTENER, UPSTREAM, name, SID).is_err());
        }
        assert!(ServiceName::parse(&"a".repeat(256)).is_some());
        assert!(ServiceName::parse(&"a".repeat(257)).is_none());
    }

    #[test]
    fn checks_sid_numeric_ranges_and_list_bounds() {
        for sid in [
            "",
            "S-2-999-42",
            "S-1-999",
            "S-1-999-",
            "S-1-999-+1",
            "S-1-0999-42",
            "S-1-999-00",
            "S-1-4294967296-42",
            "S-1-0x0000ffffffff-42",
            "S-1-0x1000000000000-42",
            "S-1-999-4294967296",
            "S-1-999-42 ",
            "S-1-999-42,",
            "S-1-999-42,s-1-999-42",
        ] {
            assert!(WindowsBrokerConfig::parse(LISTENER, UPSTREAM, SERVICE, sid).is_err());
        }
        for authority in ["4294967295", "0x000100000000", "0xFFFFFFFFFFFF"] {
            let sid = format!("S-1-{authority}{}", "-4294967295".repeat(15));
            assert!(WindowsSid::parse(&sid).is_ok());
            assert!(WindowsSid::parse(&format!("{sid}-0")).is_err());
        }
        let sids: Vec<_> = (0..33).map(|value| format!("S-1-999-{value}")).collect();
        assert!(
            WindowsBrokerConfig::parse(LISTENER, UPSTREAM, SERVICE, &sids[..32].join(",")).is_ok()
        );
        assert!(WindowsBrokerConfig::parse(LISTENER, UPSTREAM, SERVICE, &sids.join(",")).is_err());
    }
}
