#![cfg(target_family = "wasm")]

mod global_config;
mod windows_loopback;

use std::{path::PathBuf, rc::Rc, sync::Arc};

use anyhow::{Context as _, Result, anyhow};
use futures::{FutureExt as _, channel::oneshot};
use gpui::{
    App, AppContext as _, Application, AsyncApp, BorrowAppContext as _, Context, Entity, Menu,
    MenuItem, Window, WindowOptions,
};
use language::{
    LanguageRegistry, LanguageServerName, LspAdapter, LspAdapterDelegate, LspInstaller,
};
use lsp::LanguageServerBinary;
use project::project_settings::ProjectSettings;
use remote::RemoteArch;
use settings::{Settings as _, SettingsStore};
use theme::ActiveTheme as _;
use wasm_bindgen::prelude::*;
use workspace::{AppState, Workspace, WorkspaceStore};

use global_config::GlobalConfig;

#[wasm_bindgen]
pub async fn start_workspace(
    path: String,
    architecture: String,
    global_config: String,
    session_path: Option<String>,
) -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    gpui_web::init_logging();
    launch_workspace(
        PathBuf::from(path),
        architecture,
        &global_config,
        session_path.as_deref(),
    )
    .await
    .map_err(|error| JsValue::from_str(&format!("{error:#}")))
}

#[wasm_bindgen]
pub async fn prepare_workspace_switch() -> Result<bool, JsValue> {
    windows_loopback::prepare_switch()
        .await
        .map_err(|error| JsValue::from_str(&format!("{error:#}")))
}

async fn launch_workspace(
    path: PathBuf,
    architecture: String,
    global_config: &str,
    session_path: Option<&str>,
) -> Result<()> {
    let global_config =
        GlobalConfig::parse(global_config).context("Loading the global Zed configuration")?;
    let arch = match architecture.as_str() {
        "aarch64" => RemoteArch::Aarch64,
        "x86_64" => RemoteArch::X86_64,
        _ => anyhow::bail!("Unsupported remote server architecture {architecture}"),
    };
    let browser = web_sys::window().context("Missing browser window")?;
    let location = browser.location();
    let host = location
        .host()
        .map_err(|_| anyhow!("Missing editor host"))?;
    let origin = location
        .origin()
        .map_err(|_| anyhow!("Missing editor origin"))?;
    let socket_scheme = if origin.starts_with("https://") {
        "wss"
    } else {
        "ws"
    };
    let socket_origin = format!("{socket_scheme}://{host}");
    let endpoint = session_path.unwrap_or("/");
    if endpoint != "/" {
        let id = endpoint
            .strip_prefix("/_zed/workspace/")
            .and_then(|path| path.strip_suffix('/'));
        anyhow::ensure!(
            id.is_some_and(|id| id.len() == 43
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))),
            "Invalid WheelMaker workspace endpoint"
        );
    }
    let managed = endpoint != "/";
    let sql_client = wasm_rpc::RpcClient::connect(&format!("{socket_origin}{endpoint}sql-rpc"))?;
    sqlez::remote_sql::set_sql_endpoint(format!("{origin}{endpoint}sql"));
    sqlez::remote_sql::set_async_sql_client(sql_client);
    db::prepare_web_database().await?;
    let app_db = db::AppDatabase::open_in_memory("zed-web").await;
    let session = session::Session::new(
        uuid::Uuid::new_v4().to_string(),
        db::kvp::KeyValueStore::from_app_db(&app_db),
    )
    .await;

    let platform = Rc::new(gpui_web::WebPlatform::new_with_backend_and_font_fallback(
        false,
        gpui_web::WebBackendPreference::WebGpu,
        gpui_web::CanvasFontFallback::EmojiAndCjk,
    ));
    let http_client = Arc::new(platform.fetch_http_client());
    let proxy_endpoint = format!("{socket_origin}{endpoint}zed-proxy");
    let (startup, ready) = oneshot::channel();
    Application::with_platform(platform)
        .with_assets(global_config.assets.clone())
        .run(move |cx| {
            cx.set_http_client(http_client);
            cx.set_global(app_db);
            let session = cx.new(|cx| session::AppSession::new(session, cx));
            let task = initialize(session, &global_config, managed, cx)
                .map(|state| windows_loopback::open(path, arch, proxy_endpoint, state, cx));
            cx.spawn(async move |_cx| {
                let result = match task {
                    Ok(task) => task.await,
                    Err(error) => Err(error),
                };
                if startup.send(result).is_err() {
                    log::debug!("The browser workspace startup waiter was dropped");
                }
            })
            .detach();
        });
    ready
        .await
        .context("The browser workspace closed during startup")?
}

fn initialize(
    session: gpui::Entity<session::AppSession>,
    global_config: &GlobalConfig,
    managed: bool,
    cx: &mut App,
) -> Result<Arc<AppState>> {
    const VERSION: &str = env!("CARGO_PKG_VERSION");
    release_channel::init(semver::Version::parse(VERSION)?, cx);
    settings::init(cx);
    cx.update_global(|settings: &mut SettingsStore, cx| {
        settings
            .set_user_settings(
                global_config.settings.as_deref().unwrap_or(
                    r#"{"inlay_hints":{"enabled":true},"project_panel":{"dock":"left"}}"#,
                ),
                cx,
            )
            .result()
    })?;

    let fs: Arc<dyn fs::Fs> = Arc::new(fs::WasmFs);
    <dyn fs::Fs>::set_global(fs.clone(), cx);
    let languages = Arc::new(LanguageRegistry::new(cx.background_executor().clone()));
    languages.register_web_grammar("rust", || Ok(tree_sitter_rust::LANGUAGE.into()));
    languages.register_web_grammar("go", || Ok(tree_sitter_go::LANGUAGE.into()));
    languages.register_web_grammar("regex", || Ok(tree_sitter_regex::LANGUAGE.into()));
    languages.register_web_grammar("gitcommit", || Ok(tree_sitter_gitcommit::LANGUAGE.into()));
    languages.register_web_grammar("tsx", || Ok(tree_sitter_typescript::LANGUAGE_TSX.into()));
    languages.register_lsp_adapter(
        grammars::load_config("rust").name,
        Arc::new(RemoteLanguageServer("rust-analyzer")),
    );
    languages.register_lsp_adapter(
        grammars::load_config("go").name,
        Arc::new(RemoteLanguageServer("gopls")),
    );
    register_language(&languages, "rust");
    register_language(&languages, "go");
    register_language(&languages, "regex");
    register_language(&languages, "gitcommit");
    register_language(&languages, "tsx");
    let state = Arc::new(AppState {
        client: client::Client::production(cx),
        fs,
        languages,
        node_runtime: node_runtime::NodeRuntime::unavailable(),
        workspace_store: cx.new(|_| WorkspaceStore::default()),
        session,
        build_window_options: |_, _| WindowOptions::default(),
    });

    AppState::set_global(state.clone(), cx);
    if managed {
        let mut project = ProjectSettings::get_global(cx).clone();
        project.session.restore_unsaved_buffers = false;
        project.session.trust_all_worktrees = true;
        ProjectSettings::override_global(project, cx);
    } else {
        let trusted_paths = workspace::WorkspaceDb::global(cx).fetch_trusted_worktrees()?;
        project::trusted_worktrees::init(trusted_paths, cx);
    }
    theme_settings::init(
        theme::LoadThemes::All(Box::new(global_config.assets.clone())),
        cx,
    );
    global_config.load_appearance(cx)?;
    state.languages.set_theme(cx.theme().clone());
    workspace::init(state.clone(), cx);
    editor::init(cx);
    diagnostics::init(cx);
    file_finder::init(cx);
    outline::init(cx);
    outline_panel::init(cx);
    search::init(cx);
    language_tools::init(cx);
    go_to_line::init(cx);
    language_selector::init(cx);
    project_panel::init(cx);
    title_bar::init(cx);
    git_ui::init(cx);
    cx.observe_new(register_pane_toolbars).detach();
    cx.set_menus([Menu::new("Zed").items([
        MenuItem::action("Open Quickly…", workspace::ToggleFileFinder::default()),
        MenuItem::action("Find in Project", workspace::DeploySearch::default()),
        MenuItem::action("Diagnostics", diagnostics::Deploy),
        MenuItem::action("Language Servers", language_tools::lsp_button::ToggleMenu),
        MenuItem::action("Outline Panel", outline_panel::ToggleFocus),
        MenuItem::separator(),
        MenuItem::action("Toggle Left Dock", workspace::ToggleLeftDock),
        MenuItem::action("Toggle Right Dock", workspace::ToggleRightDock),
        MenuItem::action("Split Right", workspace::SplitRight::default()),
    ])]);
    global_config.load_keymap(cx)?;
    Ok(state)
}

fn register_pane_toolbars(
    workspace: &mut Workspace,
    window: Option<&mut Window>,
    cx: &mut Context<Workspace>,
) {
    let Some(window) = window else {
        return;
    };
    initialize_pane_toolbar(workspace, workspace.active_pane(), window, cx);
    cx.subscribe_in(&cx.entity(), window, |workspace, _, event, window, cx| {
        if let workspace::Event::PaneAdded(pane) = event {
            initialize_pane_toolbar(workspace, pane, window, cx);
        }
    })
    .detach();
}

fn initialize_pane_toolbar(
    workspace: &Workspace,
    pane: &Entity<workspace::Pane>,
    window: &mut Window,
    cx: &mut App,
) {
    let languages = workspace.project().read(cx).languages().clone();
    pane.update(cx, |pane, cx| {
        pane.toolbar().update(cx, |toolbar, cx| {
            let breadcrumbs = cx.new(|_| breadcrumbs::Breadcrumbs::new());
            let buffer_search =
                cx.new(|cx| search::BufferSearchBar::new(Some(languages), window, cx));
            let quick_actions = cx.new(|cx| {
                quick_action_bar::QuickActionBar::new(buffer_search.clone(), workspace, cx)
            });
            let project_search = cx.new(|_| search::project_search::ProjectSearchBar::new());
            let diagnostic_controls = cx.new(|_| diagnostics::ToolbarControls::new());
            toolbar.add_item(breadcrumbs, window, cx);
            toolbar.add_item(buffer_search, window, cx);
            toolbar.add_item(quick_actions, window, cx);
            toolbar.add_item(project_search, window, cx);
            toolbar.add_item(diagnostic_controls, window, cx);
        });
    });
}

fn register_language(registry: &LanguageRegistry, asset: &'static str) {
    let config = grammars::load_config(asset);
    registry.register_language(
        config.name.clone(),
        config.grammar.clone(),
        config.matcher.clone(),
        config.hidden,
        None,
        Arc::new(move || {
            let config = config.clone();
            async move {
                Ok(language::LoadedLanguage {
                    config,
                    queries: grammars::load_queries(asset),
                    context_provider: None,
                    toolchain_provider: None,
                    manifest_name: None,
                })
            }
            .boxed()
        }),
    );
}

// The client matches remote capabilities by language and server name.
// The remote server owns the language server binary lifecycle.
struct RemoteLanguageServer(&'static str);

impl LspAdapter for RemoteLanguageServer {
    fn name(&self) -> LanguageServerName {
        LanguageServerName::new_static(self.0)
    }
}

impl LspInstaller for RemoteLanguageServer {
    type BinaryVersion = ();

    async fn fetch_latest_server_version(
        &self,
        _: &Arc<dyn LspAdapterDelegate>,
        _: bool,
        _: &mut AsyncApp,
    ) -> Result<()> {
        anyhow::bail!("Language servers run on the remote server")
    }

    fn fetch_server_binary(
        &self,
        _: (),
        _: PathBuf,
        _: &Arc<dyn LspAdapterDelegate>,
    ) -> impl Send + std::future::Future<Output = Result<LanguageServerBinary>> + use<> {
        async { anyhow::bail!("Language servers run on the remote server") }
    }

    async fn cached_server_binary(
        &self,
        _: PathBuf,
        _: &dyn LspAdapterDelegate,
    ) -> Option<LanguageServerBinary> {
        None
    }
}
