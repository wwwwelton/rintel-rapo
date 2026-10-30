//! WebView2 host for the wasm UI. Windows-only. Assets come from `ui-wasm/pkg`,
//! produced by `scripts/build-ui.sh` before `cargo build`.

use std::borrow::Cow;
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

/// Opens a window rendering the header from `header_json` via wasm.
pub fn run_preview(header_json: &str) -> wry::Result<()> {
    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title("Rintel RAPO (webview preview)")
        .with_inner_size(tao::dpi::LogicalSize::new(960.0, 180.0))
        .build(&event_loop)
        .expect("create window");

    let _webview = WebViewBuilder::new()
        .with_custom_protocol("rapo".into(), |_id, req: Request<Vec<u8>>| asset(req.uri().path()))
        .with_url(ORIGIN)
        .with_initialization_script(&format!("window.INITIAL_HEADER = {header_json};"))
        .build(&window)?;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        if let Event::WindowEvent { event: WindowEvent::CloseRequested, .. } = event {
            *control_flow = ControlFlow::Exit;
        }
    });
}
