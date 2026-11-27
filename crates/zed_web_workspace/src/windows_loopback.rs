use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::{Context as _, Result, anyhow, bail};
use async_trait::async_trait;
use collections::HashMap;
use futures::{
    FutureExt as _, StreamExt as _,
    channel::{mpsc, oneshot},
    select,
};
use gpui::{App, AppContext as _, AsyncApp, Global, PathPromptOptions, Task};
use language_tools::lsp_button;
use remote::{
    CommandTemplate, ConnectionIdentifier, Interactive, RemoteArch, RemoteClient,
    RemoteClientDelegate, RemoteConnection, RemoteConnectionOptions, RemoteOs, RemotePlatform,
    SshConnectionOptions,
};
use rpc::proto::Envelope;
use ui::PopoverMenuHandle;
use util::paths::{PathStyle, RemotePathBuf};
use wasm_bindgen::{JsCast as _, closure::Closure};
use web_sys::{BinaryType, MessageEvent, WebSocket};
use workspace::notifications::DetachAndPromptErr as _;

struct Connection {
    arch: RemoteArch,
    url: String,
    connection_options: SshConnectionOptions,
    cancelled: AtomicBool,
    connected: Arc<AtomicBool>,
    cancellation: Mutex<Option<oneshot::Sender<()>>>,
}

impl Connection {
    fn new(url: String, arch: RemoteArch) -> Result<Self> {
        let endpoint =
            web_sys::Url::new(&url).map_err(|error| anyhow!("Invalid RPC URL: {error:?}"))?;
        // Workspace persistence uses a remote connection descriptor. The browser owns
        // the WebSocket and never constructs an SSH connection from these options.
        let connection_options = SshConnectionOptions {
            host: endpoint.hostname().into(),
            port: endpoint.port().parse().ok(),
            nickname: Some("Zed Web".to_owned()),
            ..Default::default()
        };
        Ok(Self {
            url,
            arch,
            connection_options,
            cancelled: AtomicBool::new(false),
            connected: Arc::new(AtomicBool::new(false)),
            cancellation: Mutex::new(None),
        })
    }
}

enum SocketEvent {
    Open,
    Message(Vec<u8>),
    Close,
    Error,
}

struct Socket {
    websocket: WebSocket,
    connected: Arc<AtomicBool>,
    events: mpsc::UnboundedReceiver<SocketEvent>,
    _open: Closure<dyn FnMut()>,
    _message: Closure<dyn FnMut(MessageEvent)>,
    _close: Closure<dyn FnMut()>,
    _error: Closure<dyn FnMut()>,
}

impl Socket {
    fn new(url: &str, connected: Arc<AtomicBool>) -> Result<Self> {
        let websocket = WebSocket::new(url)
            .map_err(|error| anyhow!("Opening loopback WebSocket: {error:?}"))?;
        websocket.set_binary_type(BinaryType::Arraybuffer);
        let (sender, events) = mpsc::unbounded();
        let open = Closure::<dyn FnMut()>::new({
            let sender = sender.clone();
            let connected = connected.clone();
            move || {
                connected.store(true, Ordering::Release);
                post_event(&sender, SocketEvent::Open);
            }
        });
        let close = Closure::<dyn FnMut()>::new({
            let sender = sender.clone();
            let connected = connected.clone();
            move || {
                connected.store(false, Ordering::Release);
                post_event(&sender, SocketEvent::Close);
            }
        });
        let error = Closure::<dyn FnMut()>::new({
            let sender = sender.clone();
            let connected = connected.clone();
            move || {
                connected.store(false, Ordering::Release);
                post_event(&sender, SocketEvent::Error);
            }
        });
        let message = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            let decoded = event
                .data()
                .dyn_into::<js_sys::ArrayBuffer>()
                .map_or(SocketEvent::Error, |buffer| {
                    SocketEvent::Message(js_sys::Uint8Array::new(&buffer).to_vec())
                });
            post_event(&sender, decoded);
        });
        websocket.set_onopen(Some(open.as_ref().unchecked_ref()));
        websocket.set_onclose(Some(close.as_ref().unchecked_ref()));
        websocket.set_onerror(Some(error.as_ref().unchecked_ref()));
        websocket.set_onmessage(Some(message.as_ref().unchecked_ref()));
        Ok(Self {
            websocket,
            connected,
            events,
            _open: open,
            _message: message,
            _close: close,
            _error: error,
        })
    }
}

fn post_event(sender: &mpsc::UnboundedSender<SocketEvent>, event: SocketEvent) {
    if sender.unbounded_send(event).is_err() {
        log::trace!("WebSocket event arrived after the loopback connection closed");
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        self.connected.store(false, Ordering::Release);
        self.websocket.set_onopen(None);
        self.websocket.set_onclose(None);
        self.websocket.set_onerror(None);
        self.websocket.set_onmessage(None);
        if let Err(error) = self.websocket.close() {
            log::debug!("Closing loopback WebSocket: {error:?}");
        }
    }
}

#[async_trait(?Send)]
impl RemoteConnection for Connection {
    fn start_proxy(
        &self,
        _identifier: String,
        reconnect: bool,
        incoming: mpsc::UnboundedSender<Envelope>,
        mut outgoing: mpsc::UnboundedReceiver<Envelope>,
        mut activity: mpsc::Sender<()>,
        _delegate: Arc<dyn RemoteClientDelegate>,
        cx: &mut AsyncApp,
    ) -> Task<Result<i32>> {
        if reconnect {
            return Task::ready(Err(anyhow!(
                "Reload the page to reconnect to the local server"
            )));
        }
        let (cancellation, cancelled) = oneshot::channel();
        let Ok(mut stored_cancellation) = self.cancellation.lock() else {
            return Task::ready(Err(anyhow!("Loopback cancellation lock poisoned")));
        };
        if self.has_been_killed() {
            return Task::ready(Ok(0));
        }
        *stored_cancellation = Some(cancellation);
        let url = self.url.clone();
        let connected = self.connected.clone();
        cx.spawn(async move |_| {
            let mut socket = Socket::new(&url, connected)?;
            let cancelled = cancelled.fuse();
            futures::pin_mut!(cancelled);
            select! {
                _ = cancelled => return Ok(0),
                event = socket.events.next().fuse() => {
                    if !matches!(event, Some(SocketEvent::Open)) {
                        bail!("Opening the Windows loopback connection failed");
                    }
                }
            }
            loop {
                select! {
                    _ = cancelled => return Ok(0),
                    message = outgoing.next().fuse() => {
                        let Some(message) = message else { return Ok(0) };
                        let length = u32::try_from(message.encoded_size())?;
                        let mut frame = Vec::with_capacity(length as usize + 4);
                        frame.extend_from_slice(&length.to_le_bytes());
                        message.encode_to_buffer(&mut frame)?;
                        // WebSocket cannot send an array view backed by WASM shared memory.
                        let frame = js_sys::Uint8Array::new_from_slice(&frame);
                        socket.websocket.send_with_js_u8_array(&frame).map_err(|error| anyhow!("Sending Zed RPC: {error:?}"))?;
                    }
                    event = socket.events.next().fuse() => match event {
                        Some(SocketEvent::Message(frame)) => {
                            let header = frame.get(..4).context("Missing Zed RPC length")?;
                            let length = u32::from_le_bytes(header.try_into()?);
                            if frame.len() - 4 != length as usize {
                                bail!("Invalid Zed RPC length");
                            }
                            incoming.unbounded_send(Envelope::decode_from_slice(&frame[4..])?)?;
                            if let Err(error) = activity.try_send(()) && error.is_disconnected() {
                                return Ok(0);
                            }
                        }
                        Some(SocketEvent::Open) => {}
                        Some(SocketEvent::Close) | None => return Ok(0),
                        Some(SocketEvent::Error) => bail!("Windows loopback WebSocket failed"),
                    }
                }
            }
        })
    }

    fn upload_directory(&self, _: PathBuf, _: RemotePathBuf, _: &App) -> Task<Result<()>> {
        Task::ready(Err(anyhow!(
            "Directory uploads are unavailable in the browser transport"
        )))
    }

    async fn kill(&self) -> Result<()> {
        self.cancelled.store(true, Ordering::Release);
        let cancellation = self
            .cancellation
            .lock()
            .map_err(|_| anyhow!("Loopback cancellation lock poisoned"))?
            .take();
        // Dropping the sender also wakes the receiver, so shutdown needs no separate reply.
        drop(cancellation);
        Ok(())
    }

    fn has_been_killed(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn build_command(
        &self,
        _: Option<String>,
        _: &[String],
        _: &HashMap<String, String>,
        _: Option<String>,
        _: Option<(u16, String, u16)>,
        _: Interactive,
    ) -> Result<CommandTemplate> {
        bail!("Shell commands are unavailable in the browser transport")
    }

    fn build_forward_ports_command(&self, _: Vec<(u16, String, u16)>) -> Result<CommandTemplate> {
        bail!("Port forwarding is unavailable in the browser transport")
    }

    fn connection_options(&self) -> RemoteConnectionOptions {
        self.connection_options.clone().into()
    }

    fn path_style(&self) -> PathStyle {
        PathStyle::Windows
    }
    fn remote_platform(&self) -> RemotePlatform {
        RemotePlatform {
            os: RemoteOs::Windows,
            arch: self.arch,
        }
    }
    fn remote_os_version(&self) -> Option<String> {
        None
    }
    fn shell(&self) -> String {
        "powershell.exe".into()
    }
    fn default_system_shell(&self) -> String {
        self.shell()
    }
    fn has_wsl_interop(&self) -> bool {
        false
    }
}

struct Delegate;

impl RemoteClientDelegate for Delegate {
    fn ask_password(
        &self,
        _: String,
        _: oneshot::Sender<askpass::EncryptedPassword>,
        _: oneshot::Receiver<()>,
        _: &mut AsyncApp,
    ) {
    }

    fn set_status(&self, status: Option<&str>, _: &mut AsyncApp) {
        if let Some(status) = status {
            log::info!("Windows loopback: {status}");
        }
    }
}

struct BrowserShortcuts {
    browser: web_sys::Window,
    handler: Closure<dyn FnMut(web_sys::KeyboardEvent)>,
}

impl Global for BrowserShortcuts {}

impl Drop for BrowserShortcuts {
    fn drop(&mut self) {
        if let Err(error) = self.browser.remove_event_listener_with_callback_and_bool(
            "keydown",
            self.handler.as_ref().unchecked_ref(),
            true,
        ) {
            log::warn!("Removing browser shortcuts: {error:?}");
        }
    }
}

fn install_browser_shortcuts(
    window: gpui::WindowHandle<workspace::MultiWorkspace>,
    app_state: Arc<workspace::AppState>,
    connected: Arc<AtomicBool>,
    cx: &mut AsyncApp,
) -> Result<()> {
    let browser = web_sys::window().context("Missing browser window")?;
    let callback_cx = cx.clone();
    let save_error = browser
        .document()
        .and_then(|document| document.get_element_by_id("save-error"))
        .context("Missing save error view")?;
    let handler = Closure::new(move |event: web_sys::KeyboardEvent| {
        if !event.ctrl_key() || event.alt_key() || event.shift_key() || event.meta_key() {
            return;
        }
        let key = event.key();
        if !matches!(key.as_str(), "o" | "s") {
            return;
        }

        // Handle the two shortcuts reserved by the browser before it opens
        // its file picker or saves the enclosing HTML document.
        event.prevent_default();
        event.stop_propagation();
        if event.repeat() {
            return;
        }
        if key == "s" && !connected.load(Ordering::Acquire) {
            // The HTML alert can still be presented while the GPUI connection
            // changes state and the canvas is waiting for its next frame.
            if let Err(error) = save_error.remove_attribute("hidden") {
                log::warn!("Showing disconnected save error: {error:?}");
            }
            return;
        }
        let mut callback_cx = callback_cx.clone();
        let app_state = app_state.clone();
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(error) = window.update(&mut callback_cx, |multi_workspace, window, cx| {
                let workspace = multi_workspace.workspace().clone();
                workspace.update(cx, |workspace, cx| match key.as_str() {
                    "o" => workspace::prompt_for_open_path_and_open(
                        workspace,
                        app_state.clone(),
                        PathPromptOptions {
                            files: true,
                            directories: true,
                            multiple: false,
                            prompt: None,
                        },
                        workspace::OpenPathPromptMode::SelectPath,
                        window,
                        cx,
                    ),
                    "s" => workspace
                        .save_active_item(workspace::SaveIntent::Save, window, cx)
                        .detach_and_prompt_err("Failed to save", window, cx, |_, _, _| None),
                    _ => {}
                });
            }) {
                log::warn!("Dispatching browser shortcut: {error:#}");
            }
        });
    });
    browser
        .add_event_listener_with_callback_and_bool(
            "keydown",
            handler.as_ref().unchecked_ref(),
            true,
        )
        .map_err(|error| anyhow!("Registering browser shortcuts: {error:?}"))?;
    cx.update(|cx| cx.set_global(BrowserShortcuts { browser, handler }));
    Ok(())
}

pub fn open(
    path: PathBuf,
    arch: RemoteArch,
    endpoint: String,
    app_state: Arc<workspace::AppState>,
    cx: &mut App,
) -> Task<Result<()>> {
    cx.spawn(async move |cx| {
        let connection = Arc::new(Connection::new(endpoint, arch)?);
        let connected = connection.connected.clone();
        let options = connection.connection_options();
        let (_keep_connected, cancellation) = oneshot::channel();
        let session = cx
            .update(|cx| {
                RemoteClient::new(
                    ConnectionIdentifier::setup(),
                    connection,
                    cancellation,
                    Arc::new(Delegate),
                    cx,
                )
            })
            .await?
            .context("Windows loopback connection cancelled")?;

        let (project, window) = cx.update(|cx| -> Result<_> {
            let project = project::Project::remote(
                session,
                app_state.client.clone(),
                app_state.node_runtime.clone(),
                app_state.languages.clone(),
                app_state.fs.clone(),
                true,
                cx,
            );
            let window_options = (app_state.build_window_options)(None, cx);
            let window = cx.open_window(window_options, |window, cx| {
                let workspace = cx.new(|cx| {
                    workspace::Workspace::new(None, project.clone(), app_state.clone(), window, cx)
                });
                cx.new(|cx| workspace::MultiWorkspace::new(workspace, window, cx))
            })?;
            Ok((project, window))
        })?;

        let (workspace, _) = workspace::open_remote_project_with_existing_connection(
            options,
            project,
            vec![path],
            app_state.clone(),
            window,
            None,
            None,
            cx,
        )
        .await?;
        let mut window_context = window.update(cx, |_, window, cx| window.to_async(cx))?;
        let project_panel =
            project_panel::ProjectPanel::load(workspace.downgrade(), window_context.clone())
                .await?;
        let git_panel =
            git_ui::git_panel::GitPanel::load(workspace.downgrade(), window_context.clone())
                .await?;
        let outline_panel =
            outline_panel::OutlinePanel::load(workspace.downgrade(), window_context.clone()).await?;
        workspace.update_in(&mut window_context, |workspace, window, cx| {
            workspace.add_panel(project_panel, window, cx);
            workspace.add_panel(outline_panel, window, cx);
            workspace.add_panel(git_panel, window, cx);
            workspace.finish_dock_restoration(cx);
            workspace.open_panel::<git_ui::git_panel::GitPanel>(window, cx);
            workspace.open_panel::<project_panel::ProjectPanel>(window, cx);
            let search_button = cx.new(|_| search::search_status_button::SearchButton::new());
            let lsp_menu = PopoverMenuHandle::default();
            let lsp_button =
                cx.new(|cx| lsp_button::LspButton::new(workspace, lsp_menu.clone(), window, cx));
            workspace.register_action(move |_, _: &lsp_button::ToggleMenu, window, cx| {
                lsp_menu.toggle(window, cx);
            });
            let diagnostic_summary =
                cx.new(|cx| diagnostics::items::DiagnosticIndicator::new(workspace, cx));
            let file_name = cx.new(|_| workspace::active_file_name::ActiveFileName::new());
            let cursor = cx.new(|_| go_to_line::cursor_position::CursorPosition::new(workspace));
            let language = cx.new(|_| language_selector::ActiveBufferLanguage::new(workspace));
            let blame = cx.new(|_| git_ui::GitBlameStatus::default());
            workspace.status_bar().update(cx, |status_bar, cx| {
                status_bar.add_left_item(search_button, window, cx);
                status_bar.add_left_item(lsp_button, window, cx);
                status_bar.add_left_item(diagnostic_summary, window, cx);
                status_bar.add_left_item(file_name, window, cx);
                status_bar.add_left_item(blame, window, cx);
                status_bar.add_right_item(cursor, window, cx);
                status_bar.add_right_item(language, window, cx);
            });
        })?;
        window.update(cx, |_, window, cx| {
            workspace.update(cx, |workspace, cx| {
                use gpui::Focusable as _;
                workspace.active_pane().focus_handle(cx).focus(window, cx);
                workspace.show_worktree_trust_security_modal(false, window, cx);
            });
        })?;
        install_browser_shortcuts(window, app_state, connected, cx)?;
        Ok(())
    })
}
