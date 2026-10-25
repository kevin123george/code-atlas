//! JSON-RPC 2.0 over stdio as LSP frames it: `Content-Length: N\r\n\r\n` + N bytes of JSON.

use std::io::{self, BufRead};

use serde_json::{json, Value};

pub fn encode(msg: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(msg).expect("JSON values always serialize");
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(&body);
    out
}

pub fn request(id: u64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

pub fn notification(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

pub fn response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// Reads one message. `Ok(None)` at a clean end of stream.
pub fn read_message(r: &mut impl BufRead) -> io::Result<Option<Value>> {
    let mut len: Option<usize> = None;
    let mut line = String::new();
    loop {
        line.clear();
        if r.read_line(&mut line)? == 0 {
            return if len.is_none() { Ok(None) } else { Err(io::ErrorKind::UnexpectedEof.into()) };
        }
        let l = line.trim_end_matches(['\r', '\n']);
        if l.is_empty() {
            if len.is_some() {
                break;
            }
            continue; // stray blank line between messages
        }
        if let Some((k, v)) = l.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                len = Some(v.trim().parse().map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad Content-Length"))?);
            }
        }
    }
    let mut body = vec![0; len.unwrap()];
    r.read_exact(&mut body)?;
    serde_json::from_slice(&body).map(Some).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// What arrived from a server.
#[derive(Debug, PartialEq)]
pub enum Incoming {
    Response { id: u64, result: Result<Value, String> },
    Request { id: Value, method: String, params: Value },
    Notification { method: String, params: Value },
}

pub fn classify(msg: Value) -> Option<Incoming> {
    let method = msg.get("method").and_then(Value::as_str).map(str::to_owned);
    let id = msg.get("id").cloned().filter(|v| !v.is_null());
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    match (method, id) {
        (Some(method), Some(id)) => Some(Incoming::Request { id, method, params }),
        (Some(method), None) => Some(Incoming::Notification { method, params }),
        (None, Some(id)) => {
            let id = id.as_u64()?;
            let result = match msg.get("error") {
                Some(e) => Err(e.get("message").and_then(Value::as_str).unwrap_or("error").to_owned()),
                None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
            };
            Some(Incoming::Response { id, result })
        }
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn frames_round_trip() {
        let a = request(1, "initialize", json!({"x": "ü"}));
        let b = notification("initialized", json!({}));
        let mut bytes = encode(&a);
        assert!(bytes.starts_with(b"Content-Length: "));
        bytes.extend(encode(&b));
        let mut r = Cursor::new(bytes);
        assert_eq!(read_message(&mut r).unwrap(), Some(a));
        assert_eq!(read_message(&mut r).unwrap(), Some(b));
        assert_eq!(read_message(&mut r).unwrap(), None);
    }

    #[test]
    fn length_counts_bytes_not_chars_and_extra_headers_are_ignored() {
        let body = r#"{"jsonrpc":"2.0","id":3,"result":"äö"}"#;
        let raw = format!("Content-Type: application/vscode-jsonrpc; charset=utf-8\r\ncontent-length: {}\r\n\r\n{body}", body.len());
        let msg = read_message(&mut Cursor::new(raw.into_bytes())).unwrap().unwrap();
        assert_eq!(classify(msg), Some(Incoming::Response { id: 3, result: Ok(json!("äö")) }));
    }

    #[test]
    fn truncated_message_is_an_error() {
        let mut r = Cursor::new(b"Content-Length: 50\r\n\r\n{}".to_vec());
        assert!(read_message(&mut r).is_err());
    }

    #[test]
    fn classifies_messages() {
        assert_eq!(
            classify(json!({"jsonrpc":"2.0","id":7,"error":{"code":-32601,"message":"nope"}})),
            Some(Incoming::Response { id: 7, result: Err("nope".into()) })
        );
        assert!(matches!(
            classify(json!({"jsonrpc":"2.0","id":"a","method":"workspace/configuration","params":{}})),
            Some(Incoming::Request { method, .. }) if method == "workspace/configuration"
        ));
        assert!(matches!(
            classify(json!({"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{}})),
            Some(Incoming::Notification { .. })
        ));
    }
}
