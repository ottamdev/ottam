//! A fixture MCP server for core's upstream tests.
//!
//! `ottam-test-mcp` serves over stdio; `--http` serves Streamable HTTP on a
//! random loopback port and prints its URL as the first stdout line;
//! `--fail-start` writes one stderr line and exits before speaking MCP.
//! Every mode writes `fixture ready` to stderr once it serves.
//!
//! `--http` also takes `--port N` (a fixed port rather than a random one) and
//! `--oauth`, which puts `/mcp` behind sign-in; see [`oauth`] for its own
//! flags: `--no-registration`, `--auto-approve`, `--token-ttl SECONDS`,
//! `--static-token TOKEN`, and `--authorization-endpoint URL`.

mod oauth;

use axum::http::request::Parts;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ElicitRequest,
    ElicitRequestParams, InputRequest, InputRequiredResult, JsonObject, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};
use serde_json::{json, Value};
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// How many `slow` calls the client cancelled before they finished.
static CANCELLED: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
struct Fixture;

/// Each tool and the one argument it reads, if any.
const TOOLS: &[(&str, &str, Option<&str>)] = &[
    ("echo", "returns its text", Some("text")),
    ("pid", "returns the server's process id", None),
    (
        "garble",
        "writes a line that is not UTF-8 to stderr, then returns ok",
        None,
    ),
    ("env", "returns an environment variable", Some("name")),
    (
        "roots",
        "returns the client's roots, one URI per line",
        None,
    ),
    (
        "header",
        "returns an HTTP request header (HTTP only)",
        Some("name"),
    ),
    (
        "slow",
        "waits for millis, then returns done",
        Some("millis"),
    ),
    (
        "cancelled",
        "returns how many slow calls were cancelled",
        None,
    ),
    (
        "spawn_child",
        "starts a long sleep and returns its pid",
        None,
    ),
    (
        "ask",
        "asks the user to pick a color and returns the reply",
        None,
    ),
    ("crash", "exits with status 3", None),
];

/// The question `ask` puts to the client.
fn color_question() -> ElicitRequestParams {
    serde_json::from_value(json!({
        "mode": "form",
        "message": "Pick a color",
        "requestedSchema": {
            "type": "object",
            "properties": {"color": {"type": "string", "enum": ["red", "blue"]}},
            "required": ["color"],
        },
    }))
    .expect("a valid form")
}

fn tool(name: &str, description: &str, argument: Option<&str>) -> Tool {
    let schema = match argument {
        Some(argument) => json!({
            "type": "object",
            "properties": {argument: {"type": if argument == "millis" { "integer" } else { "string" }}},
            "required": [argument],
        }),
        None => json!({"type": "object"}),
    };
    let Value::Object(schema) = schema else {
        unreachable!("schema is an object")
    };
    Tool::new(name.to_owned(), description.to_owned(), Arc::new(schema))
}

fn argument<'a>(arguments: &'a Option<JsonObject>, name: &str) -> Option<&'a Value> {
    arguments.as_ref().and_then(|arguments| arguments.get(name))
}

fn text(value: impl Into<String>) -> Result<CallToolResponse, ErrorData> {
    Ok(CallToolResult::success(vec![ContentBlock::text(value.into())]).into())
}

impl ServerHandler for Fixture {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(
            TOOLS
                .iter()
                .map(|(name, description, argument)| tool(name, description, *argument))
                .collect(),
        ))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let arguments = &request.arguments;
        let string = |name| {
            argument(arguments, name)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        match request.name.as_ref() {
            "echo" => text(string("text")),
            "pid" => text(std::process::id().to_string()),
            // `eprintln!` panics once stderr's reader is gone, so a second
            // call shows whether core still drains it.
            "garble" => {
                let mut stderr = std::io::stderr();
                stderr
                    .write_all(b"fixture wrote \xff\xfe\n")
                    .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
                eprintln!("fixture garbled");
                text("ok")
            }
            "env" => text(std::env::var(string("name")).unwrap_or_default()),
            // Roots are deprecated from protocol 2026-07-28 (SEP-2577), but
            // servers on earlier versions still ask for them.
            #[allow(deprecated)]
            "roots" => {
                let roots = context
                    .peer
                    .list_roots()
                    .await
                    .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
                let uris: Vec<_> = roots.roots.into_iter().map(|root| root.uri).collect();
                text(uris.join("\n"))
            }
            "header" => {
                let value = context
                    .extensions
                    .get::<Parts>()
                    .and_then(|parts| parts.headers.get(string("name")))
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_owned();
                text(value)
            }
            "slow" => {
                let millis = argument(arguments, "millis")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                tokio::select! {
                    () = tokio::time::sleep(Duration::from_millis(millis)) => text("done"),
                    () = context.ct.cancelled() => {
                        CANCELLED.fetch_add(1, Ordering::SeqCst);
                        Err(ErrorData::internal_error("cancelled", None))
                    }
                }
            }
            "cancelled" => text(CANCELLED.load(Ordering::SeqCst).to_string()),
            "spawn_child" => {
                let child = std::process::Command::new("sleep")
                    .arg("600")
                    .spawn()
                    .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
                text(child.id().to_string())
            }
            // Protocol 2026-07-28 forbids server-to-client requests, so a
            // session on it gets `input_required`; earlier ones get
            // `elicitation/create`. Either way the reply comes back as text.
            "ask" => {
                if let Some(reply) = request
                    .input_responses
                    .as_ref()
                    .and_then(|responses| responses.get("color"))
                {
                    return text(format!("input_required {reply}"));
                }
                let modern = context
                    .protocol_version()
                    .is_some_and(|version| !version.has_initialize());
                if modern {
                    let requests = [(
                        "color".to_owned(),
                        InputRequest::Elicitation(ElicitRequest::new(color_question())),
                    )];
                    return Ok(InputRequiredResult::from_input_requests(requests.into()).into());
                }
                let reply = context
                    .peer
                    .create_elicitation(color_question())
                    .await
                    .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
                text(format!(
                    "elicitation {}",
                    serde_json::to_string(&reply).unwrap_or_default()
                ))
            }
            "crash" => {
                eprintln!("fixture crashed on request");
                std::process::exit(3);
            }
            other => Err(ErrorData::invalid_params(
                format!("no tool named {other}"),
                None,
            )),
        }
    }
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--fail-start") => {
            eprintln!("fixture refused to start");
            std::process::exit(2);
        }
        Some("--http") => serve_http(&args[1..]).await,
        _ => serve_stdio().await,
    }
}

/// The value after `flag`, parsed, if `flag` is given.
fn flag_value<T: std::str::FromStr>(args: &[String], flag: &str) -> Option<T> {
    let index = args.iter().position(|arg| arg == flag)?;
    let value = args.get(index + 1).unwrap_or_else(|| {
        eprintln!("{flag} needs a value");
        std::process::exit(64);
    });
    Some(value.parse().unwrap_or_else(|_| {
        eprintln!("{flag} {value} is not valid");
        std::process::exit(64);
    }))
}

async fn serve_stdio() {
    let service = Fixture
        .serve(rmcp::transport::stdio())
        .await
        .expect("serves over stdio");
    eprintln!("fixture ready");
    let _ = service.waiting().await;
}

async fn serve_http(args: &[String]) {
    let port = flag_value(args, "--port").unwrap_or(0u16);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .expect("binds loopback");
    let address = listener.local_addr().expect("has an address");
    let service = StreamableHttpService::new(
        || Ok(Fixture),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    let mut router = axum::Router::new().route_service("/mcp", service);
    if args.iter().any(|arg| arg == "--oauth") {
        let has = |flag: &str| args.iter().any(|arg| arg == flag);
        let options = oauth::Options {
            registration: !has("--no-registration"),
            auto_approve: has("--auto-approve"),
            token_ttl: Duration::from_secs(flag_value(args, "--token-ttl").unwrap_or(3600)),
            static_token: flag_value(args, "--static-token"),
            authorization_endpoint: flag_value(args, "--authorization-endpoint"),
        };
        router = oauth::router(format!("http://{address}"), options, router);
    }
    let mut stdout = std::io::stdout();
    writeln!(stdout, "http://{address}/mcp").expect("prints its URL");
    stdout.flush().expect("flushes its URL");
    eprintln!("fixture ready");
    axum::serve(listener, router).await.expect("serves HTTP");
}
