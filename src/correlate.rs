//! Ids that tie log lines together — request, trace, span and session ids — found on one line,
//! so the file can be filtered to everything one request did.

use regex_automata::meta::Regex;

/// Keys whose values are ids, matched without regard to case.
pub const DEFAULT_KEYS: [&str; 18] = [
    "request_id",
    "requestId",
    "req_id",
    "x-request-id",
    "trace_id",
    "traceId",
    "span_id",
    "spanId",
    "correlation_id",
    "correlationId",
    "session_id",
    "sessionId",
    "transaction_id",
    "txn_id",
    "job_id",
    "order_id",
    "user_id",
    "userId",
];

/// Longest id value taken.
const VALUE_MAX: usize = 128;

/// An id on a line: the key it follows ("uuid" for a bare UUID) and its value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoundId {
    pub key: String,
    pub value: String,
}

/// The id patterns for a list of keys.
pub struct IdFinder {
    keyed: Option<Regex>,
    uuid: Regex,
}

impl IdFinder {
    pub fn new<S: AsRef<str>>(keys: &[S]) -> IdFinder {
        let keys: Vec<String> = keys.iter().map(|k| k.as_ref().trim()).filter(|k| !k.is_empty()).map(regex_syntax::escape).collect();
        // key = value, key: value, "key": "value", [key=value].
        let keyed = (!keys.is_empty()).then(|| format!(r#"(?i)(?:^|[^A-Za-z0-9_\-])["']?({})["']?\s*[:=]\s*["']?([A-Za-z0-9][A-Za-z0-9_.:/@\-]*)"#, keys.join("|")));
        IdFinder {
            keyed: keyed.and_then(|p| Regex::new(&p).ok()),
            uuid: Regex::new(r"(?i)\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b").expect("uuid pattern"),
        }
    }

    /// Up to `max` different ids on `line`, in order. Bare UUIDs count only when no key matched.
    pub fn find(&self, line: &str, max: usize) -> Vec<FoundId> {
        let mut out: Vec<FoundId> = Vec::new();
        let hay = line.as_bytes();
        if let Some(re) = &self.keyed {
            for caps in re.captures_iter(hay) {
                let (Some(k), Some(v)) = (caps.get_group(1), caps.get_group(2)) else { continue };
                let value = line[v.range()].trim_end_matches(['.', ':', '/', '-', '@']);
                if value.is_empty() || value.len() > VALUE_MAX || out.iter().any(|f| f.value == value) {
                    continue;
                }
                out.push(FoundId { key: line[k.range()].to_string(), value: value.to_string() });
                if out.len() >= max {
                    return out;
                }
            }
        }
        if out.is_empty() {
            for m in self.uuid.find_iter(hay) {
                let value = &line[m.range()];
                if !out.iter().any(|f| f.value.eq_ignore_ascii_case(value)) {
                    out.push(FoundId { key: "uuid".into(), value: value.to_string() });
                }
                if out.len() >= max {
                    break;
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(line: &str) -> Vec<(String, String)> {
        IdFinder::new(&DEFAULT_KEYS).find(line, 4).into_iter().map(|f| (f.key, f.value)).collect()
    }

    fn pair(k: &str, v: &str) -> (String, String) {
        (k.to_string(), v.to_string())
    }

    #[test]
    fn keyed_ids_in_common_forms() {
        assert_eq!(ids("2024-05-01 10:00:00 INFO request_id=abc123 user_id=42 done."), [pair("request_id", "abc123"), pair("user_id", "42")]);
        assert_eq!(ids(r#"{"ts":1,"traceId":"4bf92f3577b34da6","spanId": "00f067aa0ba902b7","msg":"x"}"#), [pair("traceId", "4bf92f3577b34da6"), pair("spanId", "00f067aa0ba902b7")]);
        assert_eq!(ids("[req_id: R-77.] GET /a"), [pair("req_id", "R-77")]);
        assert_eq!(ids("X-Request-ID=e1/2 ok"), [pair("X-Request-ID", "e1/2")]);
        // Part of a longer word is not the key.
        assert!(ids("myrequest_id=1 prerequest_id: 2").is_empty());
        // The same value once.
        assert_eq!(ids("trace_id=t1 traceId=t1").len(), 1);
    }

    #[test]
    fn bare_uuids_only_without_keys() {
        let u = "123e4567-e89b-12d3-a456-426614174000";
        assert_eq!(ids(&format!("job {u} started")), [pair("uuid", u)]);
        assert_eq!(ids(&format!("order_id=9 job {u}")), [pair("order_id", "9")]);
        assert!(ids("nothing here 1234").is_empty());
        let none: [&str; 0] = [];
        assert_eq!(IdFinder::new(&none).find("request_id=1", 4), Vec::new());
        assert_eq!(IdFinder::new(&["pid"]).find("pid=4711 x", 4), [FoundId { key: "pid".into(), value: "4711".into() }]);
    }

    #[test]
    fn caps_the_number_and_length() {
        let line = "trace_id=a span_id=b request_id=c session_id=d job_id=e";
        assert_eq!(IdFinder::new(&DEFAULT_KEYS).find(line, 2).len(), 2);
        let long = format!("trace_id={}", "a".repeat(200));
        assert!(ids(&long).is_empty());
    }
}
