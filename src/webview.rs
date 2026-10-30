//! WebView2 host. Build embedded assets with scripts/build-ui.sh first.
use std::{borrow::Cow, time::{Duration, Instant}};
use crate::{app::App, command::Command};
use tao::{event::{Event, WindowEvent}, event_loop::{ControlFlow, EventLoopBuilder}, window::{Icon, WindowBuilder}};
use wry::{http::{Request, Response}, WebViewBuilder};

const INDEX_HTML: &str = include_str!("../ui-wasm/index.html");
const UI_JS: &[u8] = include_bytes!("../ui-wasm/pkg/rapo_ui.js");
const UI_WASM: &[u8] = include_bytes!("../ui-wasm/pkg/rapo_ui_bg.wasm");
const ORIGIN: &str = "http://rapo.localhost/";


enum HostEvent {
    Command(Command),
    Frame,
}
fn asset(path: &str) -> Response<Cow<'static, [u8]>> {
    let (mime, body): (&str, &[u8]) = match path {
        "/" | "/index.html" => ("text/html; charset=utf-8", INDEX_HTML.as_bytes()),
        "/rapo_ui.js" => ("text/javascript", UI_JS),
        "/rapo_ui_bg.wasm" => ("application/wasm", UI_WASM),
        _ => return Response::builder().status(404).body(Cow::Borrowed(&b""[..])).expect("static response"),
    };
    Response::builder().header("Content-Type", mime).body(Cow::Borrowed(body)).expect("static asset response")
}


pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let event_loop = EventLoopBuilder::<HostEvent>::with_user_event().build();
    let window = WindowBuilder::new().with_title("Rintel RAPO")
        .with_inner_size(tao::dpi::LogicalSize::new(1180.0, 840.0))
        .with_min_inner_size(tao::dpi::LogicalSize::new(960.0, 680.0)).build(&event_loop)?;
    let mut app = Some(App::new());
    let initial = app.as_ref().unwrap().webview_state();
    let proxy = event_loop.create_proxy();
    let webview = WebViewBuilder::new()
        .with_custom_protocol("rapo".into(), |_id, req: Request<Vec<u8>>| asset(req.uri().path()))
        .with_url(ORIGIN)
        .with_navigation_handler(|url| url == ORIGIN || url == format!("{ORIGIN}index.html"))
        .with_initialization_script(&format!("window.INITIAL_STATE = {initial};"))
        .with_ipc_handler(move |req| {
            if req.body() == "{\"action\":\"frame\"}" {
                let _ = proxy.send_event(HostEvent::Frame);
            } else if let Some(command) = crate::command::from_json(req.body()) {
                let _ = proxy.send_event(HostEvent::Command(command));
            }
        }).build(&window)?;
    let mut next_tick = Instant::now();
    let mut next_render = Instant::now();
    let mut icon_active = None;
    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::WaitUntil(next_tick.min(next_render));
        match event {
            Event::LoopDestroyed => { drop(app.take()); }
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => *flow = ControlFlow::Exit,
            Event::UserEvent(HostEvent::Command(command)) => {
                if app.as_mut().unwrap().apply(command) { *flow = ControlFlow::Exit; }
                next_render = Instant::now();
            }
            Event::UserEvent(HostEvent::Frame) => {
                if !window.is_minimized() {
                    app.as_mut().unwrap().record_ui_frame();
                }
            }
            Event::MainEventsCleared => {
                let state = app.as_mut().unwrap();
                let now = Instant::now();
                if now >= next_tick {
                    state.tick(window.is_minimized());
                    next_tick = state.next_refresh();
                }
                if now >= next_render {
                    let snapshot = state.webview_state();
                    let active = snapshot["header"]["optimizer_running"].as_bool().unwrap_or(false);
                    if icon_active != Some(active) {
                        let (rgba, width, height) = crate::tray::build_window_icon_rgba(active);
                        if let Ok(icon) = Icon::from_rgba(rgba, width, height) { window.set_window_icon(Some(icon)); }
                        icon_active = Some(active);
                    }
                    if let Err(error) = webview.evaluate_script(&format!("window.rapoUpdate?.({snapshot});")) {
                        eprintln!("WebView update: {error}");
                    }
                    next_render = now + Duration::from_secs(1);
                }
            }
            _ => {}
        }
    });
}
