//! Minimal WebView2 probe for todo 0057. Same window shape as any Tauri v2
//! app: a tao top-level window hosting a WebView2 child owned by a separate
//! msedgewebview2.exe process. A big button and a text input report every
//! change to Rust via wry's IPC channel, which writes `<count>\n<text>` to
//! the state file named in argv[1] so a test on the other side of a headless
//! dock can assert the posted click/type actually landed.
//!
//! `windows_subsystem = "windows"` suppresses the console window a plain
//! Rust binary would otherwise flash on launch.
#![windows_subsystem = "windows"]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::window::WindowBuilder;
use wry::http::Request;
use wry::WebViewBuilder;

#[derive(Default)]
struct State {
    count: u32,
    text: String,
}

fn write_state(path: &PathBuf, s: &State) {
    // Best-effort: a failed write here just means the test's poll times out
    // with a clear "state file never appeared" failure instead of a panic
    // inside someone else's IPC callback.
    let _ = std::fs::write(path, format!("{}\n{}", s.count, s.text));
}

const HTML: &str = r#"<!doctype html><html><body style="margin:0;font-family:sans-serif;background:#fff">
<button id="btn" style="font-size:40px;width:100%;height:200px">clicks: <span id="n">0</span></button>
<input id="txt" style="font-size:32px;width:90%;margin:12px;display:block">
<script>
let n = 0;
document.getElementById('btn').addEventListener('click', () => {
  n++;
  document.getElementById('n').textContent = n;
  window.ipc.postMessage('click:' + n);
});
document.getElementById('txt').addEventListener('input', (e) => {
  window.ipc.postMessage('text:' + e.target.value);
});
</script></body></html>"#;

fn main() -> wry::Result<()> {
    let state_path = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("usage: webview2_probe <state_file>"),
    );
    let state = Arc::new(Mutex::new(State::default()));
    write_state(&state_path, &state.lock().unwrap());

    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title("webview2_probe")
        .with_inner_size(LogicalSize::new(900.0, 600.0))
        .build(&event_loop)
        .expect("build tao window");

    let ipc_state = state.clone();
    let ipc_path = state_path.clone();
    let handler = move |req: Request<String>| {
        let body = req.body().as_str();
        let mut s = ipc_state.lock().unwrap();
        if let Some(rest) = body.strip_prefix("click:") {
            s.count = rest.parse().unwrap_or(s.count + 1);
        } else if let Some(rest) = body.strip_prefix("text:") {
            s.text = rest.to_string();
        }
        write_state(&ipc_path, &s);
    };

    let _webview = WebViewBuilder::new()
        .with_html(HTML)
        .with_ipc_handler(handler)
        .build(&window)?;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        if let Event::WindowEvent { event: WindowEvent::CloseRequested, .. } = event {
            *control_flow = ControlFlow::Exit;
        }
    });
}
