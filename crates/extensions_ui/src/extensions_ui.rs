mod components;

use std::{any::TypeId, ops::Range, sync::Arc, time::Duration};

use cloud_api_types::{ExtensionMetadata, ExtensionProvides};
use command_palette_hooks::CommandPaletteFilter;
use editor::{Editor, EditorElement, EditorStyle};
use extension_host::{ExtensionIndexEntry, ExtensionManifest, ExtensionStore};
use fuzzy::{StringMatch, StringMatchCandidate, match_strings};
use git::{GitHostingProviderRegistry, parse_git_remote_url};
use gpui::{
    Action, App, ClipboardItem, Context, DismissEvent, Entity, EventEmitter, Focusable,
    Task, TextStyle, UniformListScrollHandle, WeakEntity, Window, actions, point, uniform_list,
};
use picker::{Picker, PickerDelegate};
use project::DirectoryLister;
use schemars::JsonSchema;
use serde::Deserialize;
use settings::Settings as _;
use theme_settings::ThemeSettings;
use ui::{ContextMenu, ListItem, ListItemSpacing, ScrollableHandle, WithScrollbar, prelude::*};
use util::ResultExt as _;
use workspace::{
    OpenPathPromptMode, Workspace,
    item::{Item, ItemEvent},
    workspace_error::{ErrorAction, ErrorSeverity, WorkspaceError},
};
use zed_actions::ExtensionCategoryFilter;

use crate::components::{ExtensionCard, extension_provides_label};

actions!(
    zed,
    [
        /// Installs an extension from a local directory for development.
        InstallDevExtension,
    ]
);

/// Rebuilds an installed dev extension.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, JsonSchema, gpui::Action)]
#[action(namespace = zed)]
#[serde(deny_unknown_fields)]
pub struct RebuildDevExtension {
    /// The ID of the dev extension to rebuild.
    ///
    /// Default: opens a picker if multiple dev extensions are installed.
    #[serde(default)]
    pub extension_id: Option<String>,
}

#[derive(Default)]
struct DevExtensionNotInstalledError {
    extension_id: Option<SharedString>,
}

impl WorkspaceError for DevExtensionNotInstalledError {
    fn primary_message(&self) -> SharedString {
        match &self.extension_id {
            Some(extension_id) => {
                format!("Dev extension '{extension_id}' is not installed.").into()
            }
            None => "No dev extensions are installed.".into(),
        }
    }

    fn primary_action(&self) -> ErrorAction {
        ErrorAction::new("Install Dev Extension", InstallDevExtension)
    }

    fn severity(&self) -> ErrorSeverity {
        ErrorSeverity::Warning
    }
}

fn update_rebuild_dev_extension_visibility(store: &Entity<ExtensionStore>, cx: &mut App) {
    let has_dev_extensions = store.read(cx).dev_extensions().next().is_some();
    CommandPaletteFilter::update_global(cx, |filter, _cx| {
        if has_dev_extensions {
            filter.show_action_types(&[TypeId::of::<RebuildDevExtension>()]);
        } else {
            filter.hide_action_types(&[TypeId::of::<RebuildDevExtension>()]);
        }
    });
}

pub fn init(cx: &mut App) {
    let store = ExtensionStore::global(cx);
    update_rebuild_dev_extension_visibility(&store, cx);
    cx.observe(&store, |store, cx| {
        update_rebuild_dev_extension_visibility(&store, cx);
    })
    .detach();

    cx.observe_new(move |workspace: &mut Workspace, window, _cx| {
        if window.is_none() {
            return;
        }
        workspace
            .register_action(
                move |workspace, action: &zed_actions::Extensions, window, cx| {
                    let provides_filter = action.category_filter.map(|category| match category {
                        ExtensionCategoryFilter::Themes => ExtensionProvides::Themes,
                        ExtensionCategoryFilter::IconThemes => ExtensionProvides::IconThemes,
                        ExtensionCategoryFilter::Languages => ExtensionProvides::Languages,
                        ExtensionCategoryFilter::Grammars => ExtensionProvides::Grammars,
                        ExtensionCategoryFilter::LanguageServers => {
                            ExtensionProvides::LanguageServers
                        }
                        ExtensionCategoryFilter::Snippets => ExtensionProvides::Snippets,
                        ExtensionCategoryFilter::DebugAdapters => ExtensionProvides::DebugAdapters,
                    });

                    let existing = workspace
                        .active_pane()
                        .read(cx)
                        .items()
                        .find_map(|item| item.downcast::<ExtensionsPage>());

                    if let Some(existing) = existing {
                        existing.update(cx, |extensions_page, cx| {
                            if provides_filter.is_some() {
                                extensions_page.change_provides_filter(provides_filter, cx);
                            }
                            if let Some(id) = action.id.as_ref() {
                                extensions_page.focus_extension(id, window, cx);
                            }
                        });

                        workspace.activate_item(&existing, true, true, window, cx);
                    } else {
                        let extensions_page = ExtensionsPage::new(
                            workspace,
                            provides_filter,
                            action.id.as_deref(),
                            window,
                            cx,
                        );
                        workspace.add_item_to_active_pane(
                            Box::new(extensions_page),
                            None,
                            true,
                            window,
                            cx,
                        )
                    }
                },
            )
            .register_action(move |workspace, _: &InstallDevExtension, window, cx| {
                let store = ExtensionStore::global(cx);
                let prompt = workspace.prompt_for_open_path(
                    gpui::PathPromptOptions {
                        files: false,
                        directories: true,
                        multiple: false,
                        prompt: None,
                    },
                    DirectoryLister::Local(
                        workspace.project().clone(),
                        workspace.app_state().fs.clone(),
                    ),
                    OpenPathPromptMode::SelectPath,
                    window,
                    cx,
                );

                let workspace_handle = cx.entity().downgrade();
                window
                    .spawn(cx, async move |cx| {
                        let extension_path = match prompt.await.map_err(anyhow::Error::from) {
                            Ok(Some(mut result)) => result.paths.pop()?,
                            Ok(None) => return None,
                            Err(err) => {
                                workspace_handle
                                    .update(cx, |workspace, cx| {
                                        workspace.show_error(
                                            workspace::workspace_error::PortalError::new(
                                                err.to_string(),
                                            ),
                                            cx,
                                        );
                                    })
                                    .ok();
                                return None;
                            }
                        };

                        let install_task = store.update(cx, |store, cx| {
                            store.install_dev_extension(extension_path, cx)
                        });

                        match install_task.await {
                            Ok(_) => {}
                            Err(err) => {
                                log::error!("Failed to install dev extension: {:?}", err);
                                workspace_handle
                                    .update(cx, |workspace, cx| {
                                        // NOTE: using `anyhow::context` here ends up not printing
                                        // the error
                                        workspace.show_error(
                                            format!("Failed to install dev extension: {}", err),
                                            cx,
                                        );
                                    })
                                    .ok();
                            }
                        }

                        Some(())
                    })
                    .detach();
            })
            .register_action(move |workspace, action: &RebuildDevExtension, window, cx| {
                if let Some(target_id) = action.extension_id.as_deref() {
                    let extension_id = ExtensionStore::global(cx)
                        .read(cx)
                        .dev_extensions()
                        .find_map(|m| {
                            if m.id.as_ref() == target_id {
                                Some(m.id.clone())
                            } else {
                                None
                            }
                        });
                    if let Some(extension_id) = extension_id {
                        ExtensionStore::global(cx).update(cx, |store, cx| {
                            store.rebuild_dev_extension(extension_id, cx);
                        });
                    } else {
                        workspace.show_error(
                            DevExtensionNotInstalledError {
                                extension_id: Some(SharedString::from(target_id.to_owned())),
                            },
                            cx,
                        );
                    }
                    return;
                }

                let dev_extensions = ExtensionStore::global(cx)
                    .read(cx)
                    .dev_extensions()
                    .cloned()
                    .collect::<Vec<_>>();

                match dev_extensions.len() {
                    0 => {
                        workspace.show_error(DevExtensionNotInstalledError::default(), cx);
                    }
                    1 => {
                        let extension_id = dev_extensions[0].id.clone();
                        ExtensionStore::global(cx).update(cx, |store, cx| {
                            store.rebuild_dev_extension(extension_id, cx);
                        });
                    }
                    _ => {
                        workspace.toggle_modal(window, cx, |window, cx| {
                            let delegate = DevExtensionRebuildPickerDelegate::new(dev_extensions);
                            Picker::uniform_list(delegate, window, cx)
                        });
                    }
                }
            });
    })
    .detach();
}

pub struct ExtensionsPage {
    extension_store: Entity<ExtensionStore>,
    provider_registry: Arc<GitHostingProviderRegistry>,
    list: UniformListScrollHandle,
    installed_search_results: Vec<ExtensionIndexEntry>,
    query_editor: Entity<Editor>,
    provides_filter: Option<ExtensionProvides>,
    local_search_task: Option<Task<()>>,
    marketplace: Option<MarketplaceResults>,
    marketplace_search_task: Option<Task<()>>,
    install_error: Option<SharedString>,
}

enum MarketplaceResults {
    Loading,
    Ready(Vec<ExtensionMetadata>),
    Failed(SharedString),
}

impl ExtensionsPage {
    pub fn new(
        workspace: &Workspace,
        provides_filter: Option<ExtensionProvides>,
        focus_extension_id: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<Self> {
        cx.new(|cx| {
            let extension_store = ExtensionStore::global(cx);
            let workspace_handle = workspace.weak_handle();
            cx.observe(&extension_store, |_: &mut Self, _, cx| cx.notify()).detach();
            cx.subscribe_in(
                &extension_store,
                window,
                move |this, _, event, window, cx| match event {
                    extension_host::Event::ExtensionsUpdated if this.marketplace.is_none() => {
                        this.update_local_search_results(cx)
                    }
                    extension_host::Event::ExtensionInstalled(id) => {
                        this.on_extension_installed(workspace_handle.clone(), id, window, cx)
                    }
                    _ => {}
                },
            ).detach();

            let query_editor = cx.new(|cx| {
                let mut input = Editor::single_line(window, cx);
                input.set_placeholder_text("Search installed extensions…", window, cx);
                if let Some(id) = focus_extension_id {
                    input.set_text(format!("id:{id}"), window, cx);
                }
                input
            });
            cx.subscribe(&query_editor, |this, _, event, cx| {
                if matches!(event, editor::EditorEvent::Edited { .. }) {
                    this.update_search_results(cx);
                }
            }).detach();
            let mut this = Self {
                extension_store,
                provider_registry: GitHostingProviderRegistry::default_global(cx),
                list: UniformListScrollHandle::new(),
                installed_search_results: Vec::new(),
                query_editor,
                provides_filter,
                local_search_task: None,
                marketplace: None,
                marketplace_search_task: None,
                install_error: None,
            };
            this.update_local_search_results(cx);
            this
        })
    }

    fn change_provides_filter(&mut self, filter: Option<ExtensionProvides>, cx: &mut Context<Self>) {
        self.provides_filter = filter;
        self.update_search_results(cx);
    }

    fn show_marketplace(&mut self, show: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.marketplace = show.then_some(MarketplaceResults::Loading);
        self.local_search_task = None;
        self.marketplace_search_task = None;
        self.query_editor.update(cx, |editor, cx| {
            let placeholder = if show {
                "Search the official extension catalog…"
            } else {
                "Search installed extensions…"
            };
            editor.set_placeholder_text(placeholder, window, cx);
        });
        self.update_search_results(cx);
    }

    fn update_search_results(&mut self, cx: &mut Context<Self>) {
        if self.marketplace.is_some() {
            let search = self.query_editor.read(cx).text(cx);
            let provides = self.provides_filter;
            let store = self.extension_store.clone();
            self.marketplace = Some(MarketplaceResults::Loading);
            self.marketplace_search_task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(250)).await;
                let result = store
                    .update(cx, |store, cx| store.fetch_marketplace_extensions(&search, provides, cx))
                    .await;
                this.update(cx, |this, cx| {
                    this.marketplace = Some(match result {
                        Ok(extensions) => MarketplaceResults::Ready(extensions),
                        Err(error) => {
                            log::error!("loading extension catalog: {error:#}");
                            MarketplaceResults::Failed(format!("Could not load the extension catalog: {error}").into())
                        }
                    });
                    this.list.set_offset(point(px(0.), px(0.)));
                    cx.notify();
                }).ok();
            }));
            cx.notify();
        } else {
            self.update_local_search_results(cx);
        }
    }

    fn install_from_marketplace(
        &mut self,
        id: Arc<str>,
        version: Arc<str>,
        cx: &mut Context<Self>,
    ) {
        self.install_error = None;
        cx.notify();
        let install = self.extension_store.update(cx, |store, cx| {
            store.install_marketplace_extension(id.clone(), version, cx)
        });
        cx.spawn(async move |this, cx| {
            if let Err(error) = install.await {
                log::error!("installing extension {id}: {error:#}");
                this.update(cx, |this, cx| {
                    this.install_error = Some(format!("Could not install {id}: {error:#}").into());
                    cx.notify();
                }).ok();
            }
        }).detach();
    }

    fn focus_extension(&self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.query_editor.update(cx, |editor, cx| {
            editor.set_text(format!("id:{id}"), window, cx);
        });
    }

    fn on_extension_installed(
        &mut self,
        workspace: WeakEntity<Workspace>,
        extension_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let extension_store = self.extension_store.read(cx);
        let themes = extension_store
            .extension_themes(extension_id)
            .map(|name| name.to_string())
            .collect::<Vec<_>>();
        if !themes.is_empty() {
            workspace
                .update(cx, |_workspace, cx| {
                    window.dispatch_action(
                        zed_actions::theme_selector::Toggle {
                            themes_filter: Some(themes),
                        }
                        .boxed_clone(),
                        cx,
                    );
                })
                .ok();
            return;
        }

        let icon_themes = extension_store
            .extension_icon_themes(extension_id)
            .map(|name| name.to_string())
            .collect::<Vec<_>>();
        if !icon_themes.is_empty() {
            workspace
                .update(cx, |_workspace, cx| {
                    window.dispatch_action(
                        zed_actions::icon_theme_selector::Toggle {
                            themes_filter: Some(icon_themes),
                        }
                        .boxed_clone(),
                        cx,
                    );
                })
                .ok();
        }
    }

    fn update_local_search_results(&mut self, cx: &mut Context<Self>) {
        let query = self.query_editor.read(cx).text(cx);
        let search = (!query.trim().is_empty()).then(|| query.trim().to_owned());
        let provides = self.provides_filter;
        let mut installed_extensions = self
            .extension_store
            .read(cx)
            .installed_extensions()
            .values()
            .filter(|entry| provides.is_none_or(|kind| entry.manifest.provides().contains(&kind)))
            .cloned()
            .collect::<Vec<_>>();

        self.local_search_task = Some(cx.spawn(async move |this, cx| {
            let results = match search.as_deref() {
                None => {
                    installed_extensions
                        .sort_by_cached_key(|extension| extension.manifest.name.to_lowercase());
                    installed_extensions
                }
                Some(search) => {
                    if let Some(extension_id) = search.strip_prefix("id:") {
                        installed_extensions
                            .into_iter()
                            .filter(|extension| extension.manifest.id.as_ref() == extension_id)
                            .collect()
                    } else {
                        let match_candidates = installed_extensions
                            .iter()
                            .enumerate()
                            .map(|(index, extension)| {
                                StringMatchCandidate::new(index, &extension.manifest.name)
                            })
                            .collect::<Vec<_>>();
                        let matches = match_strings(
                            &match_candidates,
                            search,
                            false,
                            true,
                            match_candidates.len(),
                            &Default::default(),
                            cx.background_executor().clone(),
                        )
                        .await;
                        matches
                            .into_iter()
                            .filter_map(|matched| {
                                installed_extensions.get(matched.candidate_id).cloned()
                            })
                            .collect()
                    }
                }
            };

            this.update(cx, |this, cx| {
                this.installed_search_results = results;
                this.list.set_offset(point(px(0.), px(0.)));
                cx.notify();
            })
            .ok();
        }));
    }

    fn render_text_input(
        &self,
        editor: &Entity<Editor>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let settings = ThemeSettings::get_global(cx);
        let text_style = TextStyle {
            color: if editor.read(cx).read_only(cx) {
                cx.theme().colors().text_disabled
            } else {
                cx.theme().colors().text
            },
            font_family: settings.ui_font.family.clone(),
            font_features: settings.ui_font.features.clone(),
            font_fallbacks: settings.ui_font.fallbacks.clone(),
            font_size: rems(0.875).into(),
            font_weight: settings.ui_font.weight,
            line_height: relative(1.3),
            ..Default::default()
        };

        EditorElement::new(
            editor,
            EditorStyle {
                background: cx.theme().colors().editor_background,
                local_player: cx.theme().players().local(),
                text: text_style,
                ..Default::default()
            },
        )
    }

    fn render_extensions(
        &mut self,
        range: Range<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<ExtensionCard> {
        let store = self.extension_store.read(cx);
        if let Some(MarketplaceResults::Ready(extensions)) = &self.marketplace {
            let page = cx.weak_entity();
            return range.filter_map(|index| {
                let extension = extensions.get(index)?;
                let id = extension.id.clone();
                let version = extension.manifest.version.clone();
                let page = page.clone();
                Some(ExtensionCard::for_marketplace(
                    extension,
                    store.extension_status(&id),
                    move |cx| {
                        page.update(cx, |page, cx| {
                            page.install_from_marketplace(id.clone(), version.clone(), cx);
                        }).ok();
                    },
                ))
            }).collect();
        }
        range.filter_map(|index| {
            let extension = self.installed_search_results.get(index)?;
            let manifest = extension.manifest.clone();
            let icon = manifest.repository.as_deref()
                .and_then(|url| parse_git_remote_url(self.provider_registry.clone(), url))
                .map(|(provider, _)| ui::git_hosting_provider_icon(provider.name().as_str()))
                .unwrap_or(IconName::Link);
            let card = if extension.dev {
                ExtensionCard::for_dev(manifest, store)
            } else {
                ExtensionCard::for_installed(manifest, store)
            };
            Some(card.repository_icon(icon).context_menu(|id, authors, window, cx| {
                Some(ContextMenu::build(window, cx, |menu, _, _| {
                    menu.entry("Copy Extension ID", None, move |_, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(id.to_string()));
                    })
                    .entry("Copy Author Info", None, move |_, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(authors.to_string()));
                    })
                }))
            }))
        }).collect()
    }
}

struct DevExtensionRebuildPickerDelegate {
    entries: Vec<Arc<ExtensionManifest>>,
    matches: Vec<StringMatch>,
    selected_index: usize,
}

impl DevExtensionRebuildPickerDelegate {
    fn new(manifests: Vec<Arc<ExtensionManifest>>) -> Self {
        let matches = manifests
            .iter()
            .enumerate()
            .map(|(ix, manifest)| StringMatch {
                candidate_id: ix,
                score: 0.0,
                positions: Vec::new(),
                string: manifest.name.clone(),
            })
            .collect();

        Self {
            entries: manifests,
            matches,
            selected_index: 0,
        }
    }
}

impl PickerDelegate for DevExtensionRebuildPickerDelegate {
    type ListItem = ListItem;

    fn name() -> &'static str {
        "dev-extension-rebuild"
    }

    fn match_count(&self) -> usize {
        self.matches.len()
    }

    fn selected_index(&self) -> usize {
        self.selected_index
    }

    fn set_selected_index(
        &mut self,
        ix: usize,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) {
        self.selected_index = ix;
    }

    fn selected_index_changed(
        &self,
        _ix: usize,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Option<Box<dyn Fn(&mut Window, &mut App) + 'static>> {
        None
    }

    fn update_matches(
        &mut self,
        query: String,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        let background = cx.background_executor().clone();
        let candidates = self
            .entries
            .iter()
            .enumerate()
            .map(|(ix, manifest)| StringMatchCandidate::new(ix, manifest.name.as_ref()))
            .collect::<Vec<_>>();

        cx.spawn_in(window, async move |this, cx| {
            let matches = if query.is_empty() {
                candidates
                    .into_iter()
                    .enumerate()
                    .map(|(index, candidate)| StringMatch {
                        candidate_id: index,
                        string: candidate.string,
                        positions: Vec::new(),
                        score: 0.0,
                    })
                    .collect()
            } else {
                match_strings(
                    &candidates,
                    &query,
                    false,
                    true,
                    100,
                    &Default::default(),
                    background,
                )
                .await
            };

            this.update(cx, |this, _cx| {
                this.delegate.matches = matches;
                this.delegate.selected_index = this
                    .delegate
                    .selected_index
                    .min(this.delegate.matches.len().saturating_sub(1));
            })
            .log_err();
        })
    }

    fn confirm(&mut self, _secondary: bool, _window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some(mat) = self.matches.get(self.selected_index) else {
            return;
        };

        let extension_id = self.entries[mat.candidate_id].id.clone();
        ExtensionStore::global(cx).update(cx, |store, cx| {
            store.rebuild_dev_extension(extension_id, cx);
        });

        cx.emit(DismissEvent);
    }

    fn dismissed(&mut self, _window: &mut Window, _cx: &mut Context<Picker<Self>>) {}

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        Arc::from("Rebuild dev extension…")
    }

    fn render_match(
        &self,
        ix: usize,
        selected: bool,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        let mat = self.matches.get(ix)?;
        let entry = self.entries.get(mat.candidate_id)?;

        let item = ListItem::new(("dev-extension-list-item", mat.candidate_id))
            .inset(true)
            .spacing(ListItemSpacing::Sparse)
            .toggle_state(selected)
            .child(
                h_flex()
                    .w_full()
                    .py_px()
                    .justify_between()
                    .gap_2()
                    .child(Label::new(entry.name.clone()))
                    .child(
                        Label::new(format!("{} • v{}", entry.id, entry.version))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
            );

        Some(item)
    }

    fn no_matches_text(&self, _window: &mut Window, _cx: &mut App) -> Option<SharedString> {
        Some("No dev extensions found".into())
    }
}

impl Render for ExtensionsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let categories = [
            ExtensionProvides::Themes,
            ExtensionProvides::IconThemes,
            ExtensionProvides::Languages,
            ExtensionProvides::Grammars,
            ExtensionProvides::LanguageServers,
            ExtensionProvides::Snippets,
            ExtensionProvides::DebugAdapters,
        ];
        let browsing = self.marketplace.is_some();
        let (count, status) = match &self.marketplace {
            None => (
                self.installed_search_results.len(),
                "No matching installed extensions".into(),
            ),
            Some(MarketplaceResults::Loading) => (0, "Loading official extensions…".into()),
            Some(MarketplaceResults::Ready(extensions)) => {
                (extensions.len(), "No matching official extensions".into())
            }
            Some(MarketplaceResults::Failed(error)) => (0, error.clone()),
        };
        let catalog_failed = matches!(self.marketplace, Some(MarketplaceResults::Failed(_)));
        v_flex()
            .size_full()
            .bg(cx.theme().colors().editor_background)
            .child(
                v_flex().gap_4().pt_4().px_4()
                    .child(h_flex().justify_between()
                        .child(Headline::new("Extensions").size(HeadlineSize::Large))
                        .child(Button::new("install-dev-extension", "Install Dev Extension")
                            .style(ButtonStyle::Outlined)
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(InstallDevExtension), cx)
                            })))
                    .child(h_flex().gap_2()
                        .child(Button::new("installed-tab", "Installed")
                            .toggle_state(!browsing)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.show_marketplace(false, window, cx);
                            })))
                        .child(Button::new("browse-tab", "Browse Official Extensions")
                            .toggle_state(browsing)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.show_marketplace(true, window, cx);
                            }))))
                    .child(h_flex().h_8().gap_2().px_2()
                        .border_1().border_color(cx.theme().colors().border).rounded_md()
                        .child(Icon::new(IconName::MagnifyingGlass).color(Color::Muted))
                        .child(div().flex_1().child(self.render_text_input(&self.query_editor, cx))))
                    .child(h_flex().id("extension-filters").gap_2().overflow_x_scroll()
                        .children(std::iter::once(None).chain(categories.into_iter().map(Some)).map(|kind| {
                            let label = kind.map_or("All", extension_provides_label);
                            Button::new(SharedString::from(label), label)
                                .toggle_state(self.provides_filter == kind)
                                .on_click(cx.listener(move |this, _, _, cx| this.change_provides_filter(kind, cx)))
                        })))
            )
            .child(
                v_flex().id("extension-list").px_4().size_full().overflow_y_hidden()
                    .when_some(self.install_error.clone().filter(|_| browsing), |list, error| {
                        list.child(Label::new(error).color(Color::Error).mt_4())
                    })
                    .when(count == 0, |list| list.child(Label::new(status).mt_4()))
                    .when(catalog_failed, |list| {
                        list.child(div().mt_2().child(
                            Button::new("retry-catalog", "Retry")
                                .on_click(cx.listener(|this, _, _, cx| this.update_search_results(cx))),
                        ))
                    })
                    .when(count > 0, |list| {
                        list.child(uniform_list("entries", count, cx.processor(Self::render_extensions))
                            .flex_grow_1().pb_4().track_scroll(&self.list))
                            .vertical_scrollbar_for(&self.list, window, cx)
                    }),
            )
    }
}

impl EventEmitter<ItemEvent> for ExtensionsPage {}

impl Focusable for ExtensionsPage {
    fn focus_handle(&self, cx: &App) -> gpui::FocusHandle {
        self.query_editor.read(cx).focus_handle(cx)
    }
}

impl Item for ExtensionsPage {
    type Event = ItemEvent;

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        "Extensions".into()
    }

    fn show_toolbar(&self) -> bool {
        false
    }

    fn to_item_events(event: &Self::Event, f: &mut dyn FnMut(workspace::item::ItemEvent)) {
        f(*event)
    }
}
