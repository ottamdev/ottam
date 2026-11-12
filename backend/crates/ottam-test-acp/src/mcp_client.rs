//! The least of an MCP client that calls a tool on an entry a session was
//! given: Streamable HTTP over plain loopback HTTP/1.1, the legacy
//! `initialize` lifecycle, and the entry's headers on every request, as
//! every pinned adapter sends them.

use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

const PROTOCOL_VERSION: &str = "2025-11-25";
const TIMEOUT: Duration = Duration::from_secs(30);

/// One `mcpServers` entry from `session/new` or `session/load`.
#[derive(Clone, Debug)]
pub struct Entry {
    pub url: String,
    pub headers: Vec<(String, String)>,
}

impl Entry {
    /// An ACP `McpServerHttp` entry: `{type, name, url, headers}`.
    pub fn from_acp(entry: &Value) -> Option<(String, Self)> {
        let name = entry["name"].as_str()?.to_owned();
        let url = entry["url"].as_str()?.to_owned();
        let headers = entry["headers"]
            .as_array()
            .map(|headers| {
                headers
                    .iter()
                    .filter_map(|header| {
                        Some((
                            header["name"].as_str()?.to_owned(),
                            header["value"].as_str()?.to_owned(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some((name, Self { url, headers }))
    }
}

/// Call `tool` with `arguments`: the result's text, or the error's message
/// as the agent would read it.
pub fn call_tool(entry: &Entry, tool: &str, arguments: &Value) -> Result<String, String> {
    let (initialized, session) = post(
        entry,
        None,
        &json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "ottam-test-acp", "version": "0"}
            }
        }),
    )?;
    reply(initialized)?;
    let session = session.ok_or("the server gave no session")?;
    post(
        entry,
        Some(&session),
        &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )?;
    let (called, _) = post(
        entry,
        Some(&session),
        &json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": tool, "arguments": arguments}
        }),
    )?;
    let result = reply(called)?;
    let text = result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if result["isError"] == true {
        return Err(text);
    }
    Ok(text)
}

/// A JSON-RPC reply's result, or its error's message.
fn reply(message: Option<Value>) -> Result<Value, String> {
    let message = message.ok_or("the server sent no reply")?;
    if let Some(error) = message.get("error") {
        return Err(error["message"].as_str().unwrap_or("MCP error").to_owned());
    }
    Ok(message["result"].clone())
}

/// POST one message; the reply among what came back, if any, and the
/// session the server named.
fn post(
    entry: &Entry,
    session: Option<&str>,
    message: &Value,
) -> Result<(Option<Value>, Option<String>), String> {
    let rest = entry
        .url
        .strip_prefix("http://")
        .ok_or("only http:// entries are supported")?;
    let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let path = if path.is_empty() { "/" } else { path };
    let body = serde_json::to_vec(message).expect("encodes the message");
    let mut request = format!(
        "POST {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\
         Content-Type: application/json\r\nAccept: application/json, text/event-stream\r\n\
         Content-Length: {}\r\n",
        body.len()
    );
    if let Some(session) = session {
        request.push_str(&format!(
            "Mcp-Session-Id: {session}\r\nMCP-Protocol-Version: {PROTOCOL_VERSION}\r\n"
        ));
    }
    for (name, value) in &entry.headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");

    let failed = |error: std::io::Error| format!("the MCP server could not be reached: {error}");
    let mut stream = TcpStream::connect(authority).map_err(failed)?;
    stream.set_read_timeout(Some(TIMEOUT)).map_err(failed)?;
    stream.write_all(request.as_bytes()).map_err(failed)?;
    stream.write_all(&body).map_err(failed)?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).map_err(failed)?;

    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or("the server's reply had no headers")?;
    let head = String::from_utf8_lossy(&response[..split]).into_owned();
    let mut raw = response[split + 4..].to_vec();
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or("the server's reply had no status")?;
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    let header = |name: &str| {
        headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    };
    if header("transfer-encoding").is_some_and(|value| value.eq_ignore_ascii_case("chunked")) {
        raw = dechunk(&raw);
    }
    let body = String::from_utf8_lossy(&raw).into_owned();
    if !(200..300).contains(&status) {
        return Err(format!("the MCP server answered HTTP {status}: {body}"));
    }
    let session = header("mcp-session-id");
    let event_stream = header("content-type").is_some_and(|value| value.contains("event-stream"));
    let messages: Vec<Value> = if event_stream {
        body.lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .filter_map(|data| serde_json::from_str(data.trim()).ok())
            .collect()
    } else {
        serde_json::from_str(&body).into_iter().collect()
    };
    let id = message.get("id");
    let found = messages
        .into_iter()
        .find(|reply| id.is_some() && reply.get("id") == id);
    Ok((found, session))
}

fn dechunk(mut raw: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    while let Some(end) = raw.windows(2).position(|window| window == b"\r\n") {
        let size = std::str::from_utf8(&raw[..end])
            .ok()
            .and_then(|line| usize::from_str_radix(line.split(';').next()?.trim(), 16).ok())
            .unwrap_or(0);
        if size == 0 || raw.len() < end + 2 + size {
            break;
        }
        body.extend_from_slice(&raw[end + 2..end + 2 + size]);
        raw = &raw[(end + 4 + size).min(raw.len())..];
    }
    body
}
