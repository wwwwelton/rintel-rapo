//! WebView2 host for the wasm UI. Windows-only. Assets come from `ui-wasm/pkg`,
//! produced by `scripts/build-ui.sh` before `cargo build`.

use std::borrow::Cow;
use crate::command::Command;
use tao::{
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    window::WindowBuilder,
};
use wry::{
    http::{Request, Response},
    WebViewBuilder,
};

const INDEX_HTML: &str = include_str!("../ui-wasm/index.html");
const UI_JS: &[u8] = include_bytes!("../ui-wasm/pkg/rapo_ui.js");
const UI_WASM: &[u8] = include_bytes!("../ui-wasm/pkg/rapo_ui_bg.wasm");

/// Origin WebView2 maps the `rapo` custom scheme to on Windows.
const ORIGIN: &str = "http://rapo.localhost/";

fn asset(path: &str) -> Response<Cow<'static, [u8]>> {
    let (mime, body): (&str, Cow<'static, [u8]>) = match path {
        "/" | "/index.html" => ("text/html; charset=utf-8", Cow::Owned(INDEX_HTML.as_bytes().to_vec())),
        "/rapo_ui.js" => ("text/javascript", Cow::Borrowed(UI_JS)),
        "/rapo_ui_bg.wasm" => ("application/wasm", Cow::Borrowed(UI_WASM)),
        _ => {
            return Response::builder()
                .status(404)
                .body(Cow::Borrowed(&b""[..]))
                .expect("static 404 response")
        }
    };
    Response::builder()
        .header("Content-Type", mime)
        .body(body)
        .expect("static asset response")
}

/// Decodes the JSON posted by `ui-wasm/index.html` into a command.
fn ui_event(body: &str) -> Option<Command> {
    let event: serde_json::Value = serde_json::from_str(body).ok()?;
    let action = event.get("action")?.as_str()?;
    let dir = event.get("dir").and_then(|v| v.as_str());
    let checked = event.get("checked").and_then(|v| v.as_bool());
    let pid = event.get("pid").and_then(|v| v.as_u64()).and_then(|p| u32::try_from(p).ok());
    crate::command::from_ui_event(action, dir, checked).or_else(|| crate::command::from_training_event(action, pid))
}

/// Opens a window rendering the header and game list from JSON via wasm.
/// UI clicks arrive over IPC as JSON with `action` and `dir` fields.
pub fn run_preview(header_json: &str, games_json: &str, training_json: &str) -> wry::Result<()> {
    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title("Rintel RAPO (webview preview)")
        .with_inner_size(tao::dpi::LogicalSize::new(960.0, 360.0))
        .build(&event_loop)
        .expect("create window");

    let _webview = WebViewBuilder::new()
        .with_custom_protocol("rapo".into(), |_id, req: Request<Vec<u8>>| asset(req.uri().path()))
        .with_url(ORIGIN)
        .with_initialization_script(&format!("window.INITIAL_HEADER = {header_json}; window.INITIAL_GAMES = {games_json}; window.INITIAL_TRAINING = {training_json};"))
        .with_ipc_handler(|req| match ui_event(req.body()) {
            Some(command) => eprintln!("ui command: {command:?}"),
            None => eprintln!("ui event ignored: {}", req.body()),
        })
        .build(&window)?;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        if let Event::WindowEvent { event: WindowEvent::CloseRequested, .. } = event {
            *control_flow = ControlFlow::Exit;
        }
    });
}
