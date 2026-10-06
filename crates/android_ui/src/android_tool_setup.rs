use super::*;
use android_tools::managed::{self, Dependency, Tool};
use android_tools::provision;
use gpui::{DismissEvent, KeyDownEvent, ScrollHandle};
use std::sync::atomic::{AtomicBool, Ordering};
use std::{cell::RefCell, rc::Rc};
use ui::{ButtonSize, Checkbox, TintColor, WithScrollbar};
use workspace::{DismissDecision, ModalView};

#[derive(Default)]
pub(super) struct ToolSetup {
    lines: Vec<String>,
    checking: Option<Task<()>>,
    pub(super) operation: Option<Task<()>>,
    pub(super) last_operation: Option<(Tool, &'static str)>,
    pub(super) choosing: bool,
    pub(super) native_cancel: Option<Arc<AtomicBool>>,
    pub(super) bootstrap_ready: Option<bool>,
    pub(super) bootstrap_root: Option<PathBuf>,
    check_generation: u64,
    offline: bool,
    expanded: bool,
}

impl ToolSetup {
    pub(super) fn new() -> Self {
        Self {
            expanded: false,
            ..Default::default()
        }
    }
}

pub(super) fn open(
    workspace: &mut Workspace,
    panel: Entity<AndroidPanel>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if let Some(wizard) = workspace.active_modal::<SetupWizard>(cx) {
        wizard.focus_handle(cx).focus(window, cx);
        return;
    }
    workspace.toggle_modal(window, cx, move |window, cx| {
        SetupWizard::new(panel, window, cx)
    });
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SetupStep {
    Review,
    Licenses,
    Installing,
    Ready,
}

#[derive(Clone, Copy)]
enum Maintenance {
    Validate,
    Rollback,
}

#[derive(Clone, Copy)]
enum SetupFolder {
    Jdk,
    ExistingSdk,
    SdkDestination,
}

impl SetupStep {
    fn title(self) -> &'static str {
        match self {
            Self::Review => "Welcome to Koda",
            Self::Licenses => "License agreement",
            Self::Installing => "Downloading tools",
            Self::Ready => "Setup complete",
        }
    }
}

#[derive(Default)]
struct SetupProgress {
    message: String,
    downloaded_bytes: u64,
    total_bytes: u64,
    finishing: bool,
    details: std::collections::VecDeque<String>,
}

#[derive(Clone)]
struct FocusReveal {
    focus: FocusHandle,
    previous: Rc<RefCell<Option<(FocusHandle, gpui::Size<Pixels>)>>>,
}

struct SetupWizard {
    panel: Entity<AndroidPanel>,
    focus_handle: FocusHandle,
    content_focus: FocusHandle,
    license_focus: FocusHandle,
    license_group_focus: Vec<FocusHandle>,
    step: SetupStep,
    rendered_step: SetupStep,
    rendered_settings: bool,
    rendered_error: Option<String>,
    content_scroll: ScrollHandle,
    focus_reveals: RefCell<std::collections::HashMap<&'static str, FocusReveal>>,
    license_scroll: ScrollHandle,
    show_settings: bool,
    offline: bool,
    install_sdk: bool,
    reuse_jdk: bool,
    reuse_sdk: bool,
    api_level: u32,
    jdk: Option<PathBuf>,
    sdk: Option<PathBuf>,
    sdk_destination: Option<PathBuf>,
    discovery: Option<provision::Discovery>,
    plan: Option<provision::SetupPlan>,
    installed: Option<provision::Installed>,
    acceptances: std::collections::HashSet<(String, String)>,
    selected_license: usize,
    progress: Arc<std::sync::Mutex<SetupProgress>>,
    cancel: Arc<AtomicBool>,
    task: Option<Task<()>>,
    busy: bool,
    choosing: bool,
    close_requested: bool,
    error: Option<String>,
    show_details: bool,
    _panel_subscription: Subscription,
}

impl SetupWizard {
    fn new(panel: Entity<AndroidPanel>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut wizard = Self {
            _panel_subscription: cx.observe(&panel, |_, _, cx| cx.notify()),
            panel,
            focus_handle: cx.focus_handle(),
            content_focus: cx.focus_handle().tab_index(0).tab_stop(true),
            license_focus: cx.focus_handle().tab_index(0).tab_stop(true),
            license_group_focus: Vec::new(),
            step: SetupStep::Review,
            rendered_step: SetupStep::Review,
            rendered_settings: false,
            rendered_error: None,
            content_scroll: ScrollHandle::new(),
            focus_reveals: Default::default(),
            license_scroll: ScrollHandle::new(),
            show_settings: false,
            offline: false,
            install_sdk: true,
            reuse_jdk: true,
            reuse_sdk: true,
            api_level: 36,
            jdk: None,
            sdk: None,
            sdk_destination: None,
            discovery: None,
            plan: None,
            installed: None,
            acceptances: Default::default(),
            selected_license: 0,
            progress: Arc::new(std::sync::Mutex::new(SetupProgress::default())),
            cancel: Arc::new(AtomicBool::new(false)),
            task: None,
            busy: false,
            choosing: false,
            close_requested: false,
            error: None,
            show_details: false,
        };
        wizard.detect(window, cx);
        wizard
    }

    fn detect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.rendered_error = None;
        #[cfg(test)]
        {
            self.focus_handle.focus(window, cx);
            self.discovery = Some(provision::Discovery {
                jdk: None,
                sdk: None,
                sdk_api_level: None,
                compile_sdk: Some(36),
                issues: Vec::new(),
                supported: true,
            });
            self.busy = false;
            self.error = None;
            cx.notify();
        }
        #[cfg(not(test))]
        {
            let root = self
                .panel
                .read(cx)
                .root
                .clone()
                .or_else(|| self.panel.read(cx).auto_sync_candidate(cx));
            self.busy = true;
            self.error = None;
            self.task = Some(cx.spawn_in(window, async move |wizard, cx| {
                let (result, destination) = cx
                    .background_spawn(async move {
                        (
                            provision::discover(root.as_deref()),
                            provision::default_sdk_directory(),
                        )
                    })
                    .await;
                wizard
                    .update_in(cx, |wizard, window, cx| {
                        wizard.busy = false;
                        wizard.task = None;
                        match result {
                            Ok(discovery) => {
                                wizard.jdk = discovery.jdk.clone();
                                wizard.sdk = discovery.sdk.clone();
                                wizard.api_level = discovery
                                    .compile_sdk
                                    .or(discovery.sdk_api_level)
                                    .unwrap_or(36);
                                wizard.discovery = Some(discovery);
                                if wizard.sdk_destination.is_none() {
                                    match destination {
                                        Ok(destination) => {
                                            wizard.sdk_destination = Some(destination)
                                        }
                                        Err(error) if wizard.sdk.is_none() => {
                                            wizard.error = Some(format!("{error:#}"))
                                        }
                                        Err(_) => {}
                                    }
                                }
                            }
                            Err(error) => wizard.error = Some(format!("{error:#}")),
                        }
                        if wizard.close_requested {
                            wizard.dismiss_deferred(cx);
                        } else if wizard.error.is_none() {
                            wizard.prepare_plan(window, cx);
                        }
                        cx.notify();
                    })
                    .log_err();
            }));
        }
    }

    fn begin_work(&mut self, cx: &mut Context<Self>) -> bool {
        if self.busy || self.choosing {
            return false;
        }
        self.rendered_error = None;
        let cancel = Arc::new(AtomicBool::new(false));
        let acquired = self.panel.update(cx, |panel, cx| {
            if panel.running
                || panel.syncing
                || panel.tool_setup.choosing
                || panel.debug_forward.is_some()
            {
                return false;
            }
            panel.tool_setup.choosing = true;
            panel.tool_setup.native_cancel = Some(cancel.clone());
            cx.notify();
            true
        });
        if !acquired {
            self.error = Some(
                "Finish the current Android operation and disconnect the debugger before changing tools."
                    .into(),
            );
            cx.notify();
            return false;
        }
        self.busy = true;
        self.cancel = cancel;
        self.error = None;
        true
    }

    fn finish_work(&mut self, cx: &mut Context<Self>) {
        self.busy = false;
        self.task = None;
        Self::release_native_job(&self.panel, &self.cancel, cx);
    }

    fn release_native_job(panel: &Entity<AndroidPanel>, cancel: &Arc<AtomicBool>, cx: &mut App) {
        panel.update(cx, |panel, cx| {
            if panel
                .tool_setup
                .native_cancel
                .as_ref()
                .is_some_and(|active| Arc::ptr_eq(active, cancel))
            {
                panel.tool_setup.native_cancel = None;
                panel.tool_setup.choosing = false;
                cx.notify();
            }
        });
    }

    fn prepare_plan(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.begin_work(cx) {
            return;
        }
        let options = provision::Options {
            jdk: self.reuse_jdk.then(|| self.jdk.clone()).flatten(),
            sdk: self.reuse_sdk.then(|| self.sdk.clone()).flatten(),
            sdk_destination: self.sdk_destination.clone(),
            api_level: self.api_level,
            install_sdk: self.install_sdk,
            offline: self.offline,
        };
        let cancel = self.cancel.clone();
        let cleanup_panel = self.panel.clone();
        let cleanup_cancel = cancel.clone();
        self.plan = None;
        self.acceptances.clear();
        cx.spawn_in(window, async move |wizard, cx| {
            let result = cx
                .background_spawn(async move { provision::plan(options, &cancel) })
                .await;
            gpui::AsyncApp::update(cx, |cx| {
                Self::release_native_job(&cleanup_panel, &cleanup_cancel, cx);
            });
            wizard
                .update_in(cx, |wizard, window, cx| {
                    wizard.finish_work(cx);
                    match result {
                        Ok(plan) => {
                            wizard.plan = Some(plan);
                            wizard.step = SetupStep::Review;
                            wizard.show_settings = false;
                            wizard.content_scroll.set_offset(Default::default());
                            wizard.focus_handle.focus(window, cx);
                            wizard.selected_license = 0;
                        }
                        Err(error) => wizard.error = Some(format!("{error:#}")),
                    }
                    if wizard.close_requested {
                        wizard.dismiss_deferred(cx);
                    }
                    cx.notify();
                })
                .log_err();
        })
        .detach();
        cx.notify();
    }

    fn licenses_accepted(&self) -> bool {
        self.plan.as_ref().is_some_and(|plan| {
            plan.licenses.iter().all(|license| {
                self.acceptances
                    .contains(&(license.id.clone(), license.sha256.clone()))
            })
        })
    }

    fn install(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.licenses_accepted() || !self.begin_work(cx) {
            return;
        }
        let Some(plan) = self.plan.take() else {
            self.finish_work(cx);
            return;
        };
        let acceptances = plan
            .licenses
            .iter()
            .map(|license| provision::LicenseAcceptance {
                id: license.id.clone(),
                sha256: license.sha256.clone(),
                plan_id: plan.id.clone(),
            })
            .collect::<Vec<_>>();
        self.step = SetupStep::Installing;
        self.progress = Arc::new(std::sync::Mutex::new(SetupProgress {
            message: "Preparing installation…".into(),
            total_bytes: plan.download_bytes,
            ..Default::default()
        }));
        let progress = self.progress.clone();
        let cancel = self.cancel.clone();
        let cleanup_panel = self.panel.clone();
        let cleanup_cancel = cancel.clone();
        cx.spawn_in(window, async move |wizard, cx| {
            let mut worker = cx
                .background_spawn(async move {
                    let result = provision::install(&plan, &acceptances, &cancel, |update| {
                        if let Ok(mut progress) = progress.lock() {
                            let message = update.message.chars().take(2048).collect::<String>();
                            if progress.message != message {
                                if progress.details.len() == 64 {
                                    progress.details.pop_front();
                                }
                                progress.details.push_back(message.clone());
                            }
                            progress.message = message;
                            progress.downloaded_bytes = update.downloaded_bytes;
                            progress.total_bytes = update.total_bytes;
                            progress.finishing = update.finishing;
                        }
                    });
                    (plan, result)
                })
                .boxed();
            let mut modal_alive = true;
            let (plan, result) = loop {
                match select(
                    worker,
                    cx.background_executor().timer(Duration::from_millis(100)),
                )
                .await
                {
                    Either::Left((result, _)) => break result,
                    Either::Right((_, pending)) => {
                        worker = pending;
                        if modal_alive {
                            modal_alive = wizard.update(cx, |_, cx| cx.notify()).is_ok();
                        }
                    }
                }
            };
            gpui::AsyncApp::update(cx, |cx| {
                Self::release_native_job(&cleanup_panel, &cleanup_cancel, cx);
            });
            wizard
                .update_in(cx, |wizard, window, cx| {
                    wizard.finish_work(cx);
                    match result {
                        Ok(installed) => {
                            wizard.plan = Some(plan);
                            wizard.installed = Some(installed);
                            wizard.step = SetupStep::Ready;
                            wizard.error = None;
                            wizard.panel.update(cx, |panel, cx| {
                                panel.bootstrap_completed(window, cx);
                            });
                        }
                        Err(error) => {
                            wizard.plan = None;
                            wizard.acceptances.clear();
                            wizard.step = SetupStep::Review;
                            wizard.error = Some(format!("{error:#}\nReview the settings again to retry. Verified cached downloads can be reused."));
                        }
                    }
                    if wizard.close_requested {
                        wizard.dismiss_deferred(cx);
                    }
                    cx.notify();
                })
                .log_err();
        })
        .detach();
        cx.notify();
    }

    fn choose_path(&mut self, dependency: Dependency, window: &mut Window, cx: &mut Context<Self>) {
        let folder = match dependency {
            Dependency::Jdk => SetupFolder::Jdk,
            Dependency::Sdk => SetupFolder::ExistingSdk,
        };
        self.choose_folder(folder, window, cx);
    }

    fn choose_folder(&mut self, folder: SetupFolder, window: &mut Window, cx: &mut Context<Self>) {
        let previous_error = self.error.clone();
        if !self.begin_work(cx) {
            return;
        }
        self.error = previous_error;
        self.choosing = true;
        let cleanup_panel = self.panel.clone();
        let cleanup_cancel = self.cancel.clone();
        let selected = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(
                match folder {
                    SetupFolder::Jdk => {
                        "Choose a full Java 21 JDK (contains bin/java and bin/javac)"
                    }
                    SetupFolder::ExistingSdk => "Choose an existing Android SDK folder",
                    SetupFolder::SdkDestination => "Choose the folder for Android SDK downloads",
                }
                .into(),
            ),
        });
        cx.spawn_in(window, async move |wizard, cx| {
            let result = async { anyhow::Ok(selected.await??) }.await;
            gpui::AsyncApp::update(cx, |cx| {
                Self::release_native_job(&cleanup_panel, &cleanup_cancel, cx);
            });
            wizard
                .update_in(cx, |wizard, _, cx| {
                    wizard.choosing = false;
                    wizard.finish_work(cx);
                    match result {
                        Ok(Some(paths)) => {
                            if let Some(path) = paths.into_iter().next() {
                                match folder {
                                    SetupFolder::Jdk => {
                                        wizard.jdk = Some(path);
                                        wizard.reuse_jdk = true;
                                    }
                                    SetupFolder::ExistingSdk => {
                                        wizard.sdk = Some(path);
                                        wizard.reuse_sdk = true;
                                    }
                                    SetupFolder::SdkDestination => {
                                        wizard.sdk_destination = Some(path);
                                        wizard.reuse_sdk = false;
                                    }
                                }
                                wizard.error = None;
                                wizard.plan = None;
                                wizard.acceptances.clear();
                            }
                        }
                        Ok(None) => {}
                        Err(error) => wizard.error = Some(format!("{error:#}")),
                    }
                    if wizard.close_requested {
                        wizard.dismiss_deferred(cx);
                    }
                    cx.notify();
                })
                .log_err();
        })
        .detach();
        cx.notify();
    }

    fn maintenance(&mut self, operation: Maintenance, window: &mut Window, cx: &mut Context<Self>) {
        if !self.begin_work(cx) {
            return;
        }
        self.step = SetupStep::Installing;
        self.progress = Arc::new(std::sync::Mutex::new(SetupProgress {
            message: match operation {
                Maintenance::Validate => {
                    "Validating the saved installation and integrity of its files…"
                }
                Maintenance::Rollback => {
                    "Validating and restoring the previous managed installation…"
                }
            }
            .into(),
            ..Default::default()
        }));
        let cancel = self.cancel.clone();
        let cleanup_cancel = cancel.clone();
        let cleanup_panel = self.panel.clone();
        cx.spawn_in(window, async move |wizard, cx| {
            let result = cx
                .background_spawn(async move {
                    match operation {
                        Maintenance::Validate => provision::validate_cancelled(&cancel),
                        Maintenance::Rollback => provision::rollback_cancelled(&cancel),
                    }
                })
                .await;
            gpui::AsyncApp::update(cx, |cx| {
                Self::release_native_job(&cleanup_panel, &cleanup_cancel, cx)
            });
            wizard
                .update_in(cx, |wizard, window, cx| {
                    wizard.finish_work(cx);
                    match result {
                        Ok(installed) => {
                            wizard.installed = Some(installed);
                            wizard.step = SetupStep::Ready;
                            wizard.error = None;
                            wizard
                                .panel
                                .update(cx, |panel, cx| panel.bootstrap_completed(window, cx));
                        }
                        Err(error) => {
                            wizard.step = SetupStep::Review;
                            wizard.error = Some(format!("{error:#}"));
                            wizard
                                .panel
                                .update(cx, |panel, cx| panel.refresh_tool_setup(cx));
                        }
                    }
                    if wizard.close_requested {
                        wizard.dismiss_deferred(cx);
                    }
                    cx.notify();
                })
                .log_err();
        })
        .detach();
        cx.notify();
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.choosing {
            self.close_requested = true;
            self.cancel.store(true, Ordering::Release);
            cx.notify();
        } else {
            self.dismiss_deferred(cx);
        }
    }

    fn dismiss_deferred(&mut self, cx: &mut Context<Self>) {
        super::defer_first_launch_setup(cx);
        cx.emit(DismissEvent);
    }

    fn invalidate_plan(&mut self) {
        self.plan = None;
        self.acceptances.clear();
    }

    fn next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_advance() {
            return;
        }
        match self.step {
            SetupStep::Review if self.plan.is_none() => self.prepare_plan(window, cx),
            SetupStep::Review => {
                if self
                    .plan
                    .as_ref()
                    .is_some_and(|plan| !plan.licenses.is_empty())
                {
                    self.step = SetupStep::Licenses;
                } else {
                    self.install(window, cx);
                }
            }
            SetupStep::Licenses => self.install(window, cx),
            SetupStep::Ready => {
                super::finish_first_launch_setup(cx);
                cx.emit(DismissEvent);
            }
            SetupStep::Installing => {}
        }
        cx.notify();
    }

    fn can_advance(&self) -> bool {
        !self.busy
            && !self.choosing
            && self.step != SetupStep::Installing
            && (self.step != SetupStep::Ready || self.installed.is_some())
            && (self.step != SetupStep::Licenses || self.licenses_accepted())
            && (self.step != SetupStep::Review || self.discovery.is_some())
            && (self.step != SetupStep::Review
                || self
                    .discovery
                    .as_ref()
                    .is_some_and(|discovery| discovery.supported)
                || (self.reuse_jdk
                    && self.jdk.is_some()
                    && (!self.install_sdk || (self.reuse_sdk && self.sdk.is_some()))))
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.choosing {
            return;
        }
        if self.step == SetupStep::Licenses {
            self.step = SetupStep::Review;
        }
        cx.notify();
    }

    fn text(text: impl Into<SharedString>) -> gpui::Div {
        div().text_sm().w_full().child(text.into())
    }

    fn move_focus(&self, backwards: bool, window: &mut Window, cx: &mut App) {
        if backwards {
            window.focus_prev(cx);
        } else {
            window.focus_next(cx);
        }
        if self.focus_handle.contains_focused(window, cx) {
            return;
        }
        self.focus_handle.focus(window, cx);
        if !backwards {
            window.focus_next(cx);
            return;
        }
        // GPUI's tab map also contains the workspace behind the modal. Find the
        // last wizard control without letting a backward wrap focus that workspace.
        let mut first = None;
        let mut last = self.focus_handle.clone();
        for _ in 0..256 {
            window.focus_next(cx);
            if !self.focus_handle.contains_focused(window, cx) {
                break;
            }
            let Some(focused) = window.focused(cx) else {
                break;
            };
            if first.as_ref() == Some(&focused) {
                break;
            }
            if first.is_none() {
                first = Some(focused.clone());
            }
            last = focused;
        }
        last.focus(window, cx);
    }

    fn scroll_key(handle: &ScrollHandle, key: &str) -> bool {
        let offset = handle.offset();
        let maximum = handle.max_offset().y;
        let page = (handle.bounds().size.height - px(24.)).max(px(24.));
        let target = match key {
            "up" => offset.y + px(24.),
            "down" => offset.y - px(24.),
            "pageup" => offset.y + page,
            "pagedown" => offset.y - page,
            "home" => px(0.),
            "end" => -maximum,
            _ => return false,
        };
        handle.set_offset(gpui::point(offset.x, target.clamp(-maximum, px(0.))));
        true
    }

    /// Reveal a newly focused control without fighting manual scrolling or the
    /// license document's independent scroll position. Wrappers are not tab stops.
    fn focus_row(
        &self,
        id: &'static str,
        child: impl IntoElement,
        cx: &mut App,
    ) -> gpui::Stateful<gpui::Div> {
        let entry = self
            .focus_reveals
            .borrow_mut()
            .entry(id)
            .or_insert_with(|| FocusReveal {
                focus: cx.focus_handle().tab_stop(false),
                previous: Default::default(),
            })
            .clone();
        let focus = entry.focus.clone();
        let scroll = self.content_scroll.clone();
        div()
            .on_children_prepainted(move |bounds, window, cx| {
                if !entry.focus.contains_focused(window, cx) {
                    *entry.previous.borrow_mut() = None;
                    return;
                }
                let Some(focused) = window.focused(cx) else {
                    return;
                };
                let viewport = scroll.bounds();
                let current = (focused.clone(), viewport.size);
                if entry.previous.borrow().as_ref() == Some(&current) {
                    return;
                }
                *entry.previous.borrow_mut() = Some(current);
                let Some(control) = bounds.first() else {
                    return;
                };
                let top = viewport.top() + px(8.);
                let bottom = viewport.bottom() - px(8.);
                let delta = if control.size.height > bottom - top || control.top() < top {
                    top - control.top()
                } else if control.bottom() > bottom {
                    bottom - control.bottom()
                } else {
                    return;
                };
                let scroll = scroll.clone();
                let entry = entry.clone();
                window.on_next_frame(move |window, cx| {
                    // A queued reveal must not move the pane after focus or layout changes.
                    if window.focused(cx).as_ref() != Some(&focused)
                        || !entry.focus.contains_focused(window, cx)
                        || entry.previous.borrow().as_ref()
                            != Some(&(focused.clone(), viewport.size))
                        || scroll.bounds().size != viewport.size
                    {
                        return;
                    }
                    let offset = scroll.offset();
                    scroll.set_offset(gpui::point(
                        offset.x,
                        (offset.y + delta).clamp(-scroll.max_offset().y, px(0.)),
                    ));
                    window.refresh();
                });
            })
            .id(&focus)
            .track_focus(&focus)
            .child(child)
    }

    fn checkbox(
        &self,
        id: &'static str,
        selected: bool,
        label: String,
        disabled: bool,
        changed: impl Fn(&mut Self, bool, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.focus_row(
            id,
            h_flex()
                .id(id)
                .debug_selector(move || id.into())
                .tab_index(0isize)
                .tab_stop(!disabled)
                .role(gpui::Role::CheckBox)
                .aria_label(format!(
                    "{label}, {}",
                    if selected { "checked" } else { "unchecked" }
                ))
                .gap_2()
                .items_start()
                .p_1()
                .rounded_md()
                .border_1()
                .border_color(gpui::transparent_black())
                .hover(|style| style.bg(cx.theme().colors().element_hover))
                .focus_visible(|style| style.border_color(cx.theme().colors().border_focused))
                .when(!disabled, |element| element.cursor_pointer())
                .on_click(cx.listener(move |wizard, _, _, cx| {
                    if !disabled {
                        changed(wizard, !selected, cx);
                    }
                }))
                .child(
                    Checkbox::new(format!("{id}-indicator"), selected.into())
                        .visualization_only(true)
                        .disabled(disabled),
                )
                .child(Self::text(label)),
            cx,
        )
        .into_any_element()
    }

    fn review(&self, cx: &mut Context<Self>) -> AnyElement {
        let disabled = self.busy || self.choosing;
        v_flex().gap_4()
            .when(!self.show_settings, |element| element.child(Label::new("Set up Java and the Android SDK to get started.").color(Color::Muted)))
            .when(self.busy, |element| element.child(Label::new("Checking installed tools…").color(Color::Muted)))
            .when(self.show_settings, |element| element
                .child(h_flex().items_start().child(div().debug_selector(|| "android-setup-change-control".into()).child(self.focus_row("reveal-android-setup-change-settings", setup_button("android-setup-change-settings", "Back to setup", cx).disabled(disabled).tab_index(0isize)
                    .start_icon(Icon::new(IconName::ArrowLeft).size(IconSize::Small))
                    .on_click(cx.listener(|wizard, _, _, cx| { wizard.show_settings = false; wizard.content_scroll.set_offset(Default::default()); cx.notify(); })), cx))))
                .child(self.settings(cx)).child(self.advanced(cx)))
            .when(!self.show_settings, |element| element
            .when_some(self.plan.as_ref(), |element, plan| {
                let java_bytes = plan.downloads.iter().filter(|download| download.label.contains("JDK")).map(|download| download.bytes).sum::<u64>();
                let sdk_bytes = plan.download_bytes.saturating_sub(java_bytes);
                element
                    .child(v_flex().debug_selector(|| "android-setup-download-summary".into()).gap_4().py_3()
                        .child(h_flex().w_full().gap_3().justify_between()
                            .child(v_flex().flex_1().min_w_0().gap_1().child(Label::new("Java 21")).child(Label::new(if java_bytes > 0 { "Eclipse Temurin · Long-term support" } else { "Existing development kit" }).size(LabelSize::Small).color(Color::Muted)))
                            .child(Label::new(if java_bytes > 0 { format_bytes(java_bytes) } else { "Installed".into() }).color(if java_bytes > 0 { Color::Muted } else { Color::Success })))
                        .when(plan.sdk.is_some(), |element| element.child(h_flex().w_full().gap_3().justify_between()
                            .child(v_flex().flex_1().min_w_0().gap_1().child(Label::new("Android SDK")).child(Label::new(format!("Platform {} · Build and device tools", self.api_level)).size(LabelSize::Small).color(Color::Muted)))
                            .child(Label::new(if sdk_bytes > 0 { format_bytes(sdk_bytes) } else { "Installed".into() }).color(if sdk_bytes > 0 { Color::Muted } else { Color::Success }))))
                        .child(ui::Divider::horizontal())
                        .child(Label::new(if plan.download_bytes > 0 { format!("Download size: {}", format_bytes(plan.download_bytes)) } else { "No downloads needed".into() }).color(Color::Muted))
                        .when(plan.sdk_is_shared, |element| element.child(Label::new("SDK shared with Android Studio").size(LabelSize::Small).color(Color::Muted))))
                    .child(h_flex().items_start().child(div().debug_selector(|| "android-setup-summary-details-control".into()).child(self.focus_row("reveal-android-setup-download-details", setup_button("android-setup-download-details", if self.show_details { "Hide details" } else { "Details" }, cx).disabled(disabled).tab_index(0isize)
                        .end_icon(Icon::new(if self.show_details { IconName::ChevronUp } else { IconName::ChevronDown }).size(IconSize::Small))
                        .on_click(cx.listener(|wizard, _, _, cx| { wizard.show_details = !wizard.show_details; cx.notify(); })), cx))))
                    .when(self.show_details, |element| element.child(v_flex().debug_selector(|| "android-setup-runtime-details".into()).gap_2()
                        .child(Self::text(format!("Java 21: {}", plan.jdk.display())))
                        .when_some(plan.sdk.as_ref(), |element, sdk| element.child(Self::text(format!("Android SDK: {}", sdk.display()))))
                        .when(plan.sdk_is_shared, |element| element.child(Self::text("Shared with Android Studio. Only missing packages are added; completed packages remain installed if setup is cancelled or restored.")))
                        .child(Self::text(format!("Supported platforms: {}", plan.supported_platform)))
                        .children(plan.downloads.iter().map(|download| Self::text(format!("{} · {}", download_label(download), format_bytes(download.bytes)))))
                        .children(plan.downloads.iter().map(|download| Self::text(format!("{} · {}", download.publisher, download.url)).text_color(cx.theme().colors().text_muted)))
                        .children(plan.provenance.iter().map(|provenance| Self::text(provenance.clone())))))
            })
            .when(self.plan.is_none() && !self.busy, |element| element
                .child(Self::text("Refresh the summary to review the components before downloading."))))
            .when_some(self.discovery.as_ref(), |element, discovery| element
                .children(discovery.issues.iter().filter(|issue| {
                    !(self.plan.is_some() && issue.starts_with("Java: Run Android: Setup"))
                        && !(self.plan.as_ref().is_some_and(|plan| plan.sdk.is_some()) && issue.starts_with("No complete Android SDK found."))
                }).map(|issue| Self::text(issue.clone())))
                .when(!discovery.supported, |element| element.child(Self::text("Automatic downloads are unavailable on this platform. Choose existing compatible tools in Customize."))))
            .when(!self.show_settings, |element| element.child(h_flex().items_start().child(div().debug_selector(|| "android-setup-change-control".into()).child(self.focus_row("reveal-android-setup-change-settings", setup_button("android-setup-change-settings", "Customize…", cx).disabled(disabled).tab_index(0isize)
                .end_icon(Icon::new(IconName::ChevronRight).size(IconSize::Small))
                .on_click(cx.listener(|wizard, _, _, cx| { wizard.show_settings = true; wizard.content_scroll.set_offset(Default::default()); cx.notify(); })), cx)))))
            .into_any_element()
    }

    fn settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let disabled = self.busy || self.choosing;
        let wizard = cx.weak_entity();
        v_flex()
            .items_start()
            .gap_4()
            .child(
                self.focus_row(
                    "jdk-choice",
                    h_flex()
                        .w_full()
                        .gap_3()
                        .justify_between()
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_1()
                                .child(Label::new("Java 21"))
                                .child(
                                    Label::new("Use an installed development kit.")
                                        .size(LabelSize::Small)
                                        .color(Color::Muted),
                                ),
                        )
                        .child(
                            setup_button("android-setup-choose-jdk", "Choose…", cx)
                                .aria_label("Choose existing JDK 21")
                                .disabled(disabled)
                                .tab_index(0isize)
                                .end_icon(Icon::new(IconName::Folder).size(IconSize::Small))
                                .on_click(cx.listener(|wizard, _, window, cx| {
                                    wizard.choose_path(Dependency::Jdk, window, cx)
                                })),
                        ),
                    cx,
                )
                .w_full(),
            )
            .when_some(self.jdk.as_ref(), |element, _| {
                element.child(self.checkbox(
                    "android-setup-reuse-jdk",
                    self.reuse_jdk,
                    "Use installed JDK".into(),
                    disabled,
                    |wizard, selected, cx| {
                        wizard.reuse_jdk = selected;
                        wizard.invalidate_plan();
                        cx.notify();
                    },
                    cx,
                ))
            })
            .when(self.jdk.is_none() || !self.reuse_jdk, |element| {
                element.child(
                    Self::text("Download Eclipse Temurin into Koda’s application storage.")
                        .text_color(cx.theme().colors().text_muted),
                )
            })
            .child(ui::Divider::horizontal())
            .child(self.checkbox(
                "android-setup-sdk",
                self.install_sdk,
                "Android SDK".into(),
                disabled,
                |wizard, selected, cx| {
                    wizard.install_sdk = selected;
                    wizard.invalidate_plan();
                    cx.notify();
                },
                cx,
            ))
            .when(self.install_sdk, |element| {
                element
                    .child(
                        Label::new("Reuse your Android Studio SDK or add missing packages.")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .when_some(self.sdk.as_ref(), |element, _| {
                        element.child(self.checkbox(
                            "android-setup-reuse-sdk",
                            self.reuse_sdk,
                            "Use installed SDK".into(),
                            disabled,
                            |wizard, selected, cx| {
                                wizard.reuse_sdk = selected;
                                wizard.invalidate_plan();
                                cx.notify();
                            },
                            cx,
                        ))
                    })
                    .child(
                        self.focus_row(
                            "sdk-location",
                            h_flex()
                                .w_full()
                                .gap_3()
                                .justify_between()
                                .child(Label::new("SDK location"))
                                .child(
                                    div()
                                        .debug_selector(|| {
                                            "android-setup-summary-sdk-folder-control".into()
                                        })
                                        .child(
                                            setup_button(
                                                "android-setup-sdk-destination",
                                                "Change…",
                                                cx,
                                            )
                                            .aria_label("Choose Android SDK folder")
                                            .disabled(disabled)
                                            .tab_index(0isize)
                                            .end_icon(
                                                Icon::new(IconName::Folder).size(IconSize::Small),
                                            )
                                            .on_click(
                                                cx.listener(|wizard, _, window, cx| {
                                                    wizard.choose_folder(
                                                        SetupFolder::SdkDestination,
                                                        window,
                                                        cx,
                                                    )
                                                }),
                                            ),
                                        ),
                                ),
                            cx,
                        )
                        .w_full(),
                    )
                    .child(
                        Self::text(
                            self.sdk
                                .as_ref()
                                .filter(|_| self.reuse_sdk)
                                .or(self.sdk_destination.as_ref())
                                .map(|path| path.display().to_string())
                                .unwrap_or_else(|| "Choose a folder".into()),
                        )
                        .text_color(cx.theme().colors().text_muted),
                    )
                    .child(
                        self.focus_row(
                            "sdk-api",
                            h_flex()
                                .w_full()
                                .gap_2()
                                .justify_between()
                                .flex_wrap()
                                .child(Label::new("Android API"))
                                .child(
                                    PopoverMenu::new("android-setup-api")
                                        .trigger(
                                            setup_button(
                                                "android-setup-api-trigger",
                                                format!("API {} ▾", self.api_level),
                                                cx,
                                            )
                                            .disabled(disabled)
                                            .tab_index(0isize),
                                        )
                                        .menu(move |window, cx| {
                                            Some(ContextMenu::build(
                                                window,
                                                cx,
                                                |mut menu, _, _| {
                                                    for api_level in [36, 37] {
                                                        let wizard = wizard.clone();
                                                        menu = menu.entry(
                                                            format!("Android API {api_level}"),
                                                            None,
                                                            move |_, cx| {
                                                                wizard
                                                                    .update(cx, |wizard, cx| {
                                                                        wizard.api_level =
                                                                            api_level;
                                                                        wizard.invalidate_plan();
                                                                        cx.notify();
                                                                    })
                                                                    .log_err();
                                                            },
                                                        );
                                                    }
                                                    menu
                                                },
                                            ))
                                        }),
                                )
                                .child(
                                    setup_button(
                                        "android-setup-choose-sdk",
                                        "Use existing SDK…",
                                        cx,
                                    )
                                    .disabled(disabled)
                                    .tab_index(0isize)
                                    .on_click(cx.listener(
                                        |wizard, _, window, cx| {
                                            wizard.choose_path(Dependency::Sdk, window, cx)
                                        },
                                    )),
                                ),
                            cx,
                        )
                        .w_full(),
                    )
            })
            .child(ui::Divider::horizontal())
            .child(self.checkbox(
                "android-setup-offline",
                self.offline,
                "Use cached downloads only (offline)".into(),
                disabled,
                |wizard, selected, cx| {
                    wizard.offline = selected;
                    wizard.invalidate_plan();
                    cx.notify();
                },
                cx,
            ))
            .child(
                Label::new("Changes are reviewed before anything is downloaded.")
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .child(
                Self::text(provision::platform_label()).text_color(cx.theme().colors().text_muted),
            )
            .into_any_element()
    }

    fn license_label(id: &str) -> &str {
        match id {
            "android-sdk-license" => "Android SDK license",
            id => id,
        }
    }

    fn licenses(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(plan) = &self.plan else {
            return Self::text("Go back and prepare an installation plan.").into_any_element();
        };
        self.license_group_focus
            .resize_with(plan.licenses.len(), || {
                cx.focus_handle().tab_index(0).tab_stop(true)
            });
        let selected = plan.licenses.get(self.selected_license);
        v_flex().gap_3()
            .child(Label::new("Review the terms before downloading.").color(Color::Muted))
            .child(self.focus_row("license-groups", h_flex().gap_1().flex_wrap().children(plan.licenses.iter().zip(&self.license_group_focus).enumerate().map(|(index, (license, focus))| {
                let accepted = self.acceptances.contains(&(license.id.clone(), license.sha256.clone()));
                div().debug_selector(move || format!("android-license-group-{index}"))
                    .child(setup_button(format!("android-license-{index}"), format!("{}{}", Self::license_label(&license.id), if accepted { " ✓" } else { "" }), cx)
                    .style(if self.selected_license == index { ButtonStyle::Filled } else { ButtonStyle::Subtle })
                    .track_focus(focus)
                    .tab_index(0isize)
                    .on_click(cx.listener(move |wizard, _, _, cx| { wizard.selected_license = index; wizard.license_scroll.set_offset(Default::default()); cx.notify(); })))
            })), cx))
            .when_some(selected, |element, license| {
                let key = (license.id.clone(), license.sha256.clone());
                let accepted = self.acceptances.contains(&key);
                element
                    .child(self.focus_row("license-document", div().id("android-setup-license-frame").debug_selector(|| "android-setup-license-frame".into()).h(px(260.)).track_focus(&self.license_focus).role(gpui::Role::Document).aria_label(Self::license_label(&license.id).to_owned()).aria_description("Use arrow keys, Page Up, Page Down, Home and End to read the license.").occlude().border_1().border_color(cx.theme().colors().border).focus_visible(|style| style.border_color(cx.theme().colors().border_focused)).rounded_md()
                        .on_click(cx.listener(|wizard, _, window, cx| wizard.license_focus.focus(window, cx)))
                        .child(div().id("android-setup-license-text").debug_selector(|| "android-setup-license-text".into()).size_full().overflow_y_scroll().track_scroll(&self.license_scroll).p_3().child(Self::text(license.text.clone()).debug_selector(|| "android-setup-license-document".into())))
                        .custom_scrollbars(ui::Scrollbars::always_visible(ui::ScrollAxes::Vertical).tracked_scroll_handle(&self.license_scroll).tracked_entity(cx.entity_id()), window, cx), cx))
                    .child(self.focus_row("license-source", h_flex().items_start().child(setup_button("android-license-source", "View license source", cx).tab_index(0isize).on_click({
                        let source = license.source.clone();
                        move |_, _, cx| cx.open_url(&source)
                    })), cx))
                    .child(self.checkbox("android-setup-license-accept", accepted,
                        format!("I accept {}", Self::license_label(&license.id)), false,
                        move |wizard, selected, cx| {
                            if selected { wizard.acceptances.insert(key.clone()); } else { wizard.acceptances.remove(&key); }
                            cx.notify();
                        }, cx))
            })
            .into_any_element()
    }

    fn installing(&self, cx: &mut Context<Self>) -> AnyElement {
        let Ok(progress) = self.progress.lock() else {
            return Self::text("Installation is running. Waiting for the next progress update…")
                .into_any_element();
        };
        v_flex().gap_3()
            .child(Label::new(if progress.finishing { "Finishing setup…" } else if self.close_requested { "Cancelling safely…" } else { "Preparing your development tools" }).size(LabelSize::Large))
            .child(Self::text(progress.message.clone()))
            .when(progress.total_bytes > 0, |element| element
                .child(ui::ProgressBar::new("android-setup-progress", progress.downloaded_bytes.min(progress.total_bytes) as f32, progress.total_bytes as f32, cx))
                .child(Label::new(format!("{} / {} downloaded", format_bytes(progress.downloaded_bytes), format_bytes(progress.total_bytes))).color(Color::Muted)))
            .child(h_flex().items_start().child(self.focus_row("reveal-android-setup-details", setup_button("android-setup-details", if self.show_details { "Hide details" } else { "Show details" }, cx).tab_index(0isize)
                .on_click(cx.listener(|wizard, _, _, cx| { wizard.show_details = !wizard.show_details; cx.notify(); })), cx)))
            .when(self.show_details, |element| element.child(Self::text("Cancellation preserves the previous selection. Completed shared SDK packages stay installed; saving already in progress finishes atomically.")).child(v_flex().id("android-setup-install-details").gap_1().max_h(px(180.)).overflow_y_scroll().children(progress.details.iter().map(|line| Self::text(line.clone())))))
            .into_any_element()
    }

    fn ready(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex().gap_3()
            .child(Label::new(if self.installed.as_ref().is_some_and(|installed| installed.sdk.is_some()) { "Java and Android SDK are ready." } else { "Java 21 is ready." }).size(LabelSize::Large).color(Color::Success))
            .child(Label::new("Your settings are saved for future launches.").color(Color::Muted))
            .child(h_flex().items_start().child(self.focus_row("reveal-android-setup-ready-details", setup_button("android-setup-ready-details", if self.show_details { "Hide details" } else { "Details" }, cx).tab_index(0isize)
                .on_click(cx.listener(|wizard, _, _, cx| { wizard.show_details = !wizard.show_details; cx.notify(); })), cx)))
            .when(self.show_details, |element| element.when_some(self.installed.as_ref(), |element, installed| {
                element
                    .child(Self::text(format!("Java 21: {}", installed.jdk.display())))
                    .when_some(installed.sdk.as_ref(), |element, sdk| element.child(Self::text(format!("Android SDK: {}", sdk.display()))))
                    .when(installed.sdk.is_none(), |element| element.child(Self::text("An Android SDK is still required to build, run and debug Android projects.")))
            })
            .child(Self::text("Preview uses the selected Java 21. Run needs an authorized device; Debug also needs the debugger runtime. Kotlin server and debugger provisioning supports Apple Silicon macOS."))
            .child(self.advanced(cx)))
            .into_any_element()
    }

    fn advanced(&self, cx: &mut Context<Self>) -> AnyElement {
        let expanded = self.panel.read(cx).tool_setup.expanded;
        let disabled = self.busy || self.choosing;
        v_flex().gap_3()
            .child(self.panel.update(cx, |panel, cx| panel.render_tool_setup(self, cx).into_any_element()))
            .when(expanded, |element| element.child(v_flex().gap_2()
                .child(Label::new("Managed Java and Android SDK"))
                .child(Self::text("Validate checks the saved files. Repair and update creates a new download summary. Restore previous restores the last verified selection; shared SDK packages are retained."))
                .child(h_flex().gap_1().flex_wrap()
                    .child(self.focus_row("reveal-android-native-validate", setup_button("android-native-validate", "Validate installation", cx).disabled(disabled).tab_index(0isize)
                        .on_click(cx.listener(|wizard, _, window, cx| wizard.maintenance(Maintenance::Validate, window, cx))), cx))
                    .child(self.focus_row("reveal-android-native-repair", setup_button("android-native-repair", "Repair / update…", cx).disabled(disabled).tab_index(0isize)
                        .on_click(cx.listener(|wizard, _, _, cx| {
                            wizard.reuse_jdk = false;
                            wizard.reuse_sdk = false;
                            wizard.plan = None;
                            wizard.acceptances.clear();
                            wizard.step = SetupStep::Review;
                            wizard.error = None;
                            cx.notify();
                        })), cx))
                    .child(self.focus_row("reveal-android-native-rollback", setup_button("android-native-rollback", "Restore previous installation", cx).disabled(disabled).tab_index(0isize)
                        .on_click(cx.listener(|wizard, _, window, cx| wizard.maintenance(Maintenance::Rollback, window, cx))), cx)))))
            .into_any_element()
    }
}

impl Drop for SetupWizard {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

impl EventEmitter<DismissEvent> for SetupWizard {}
impl Focusable for SetupWizard {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
impl ModalView for SetupWizard {
    fn on_before_dismiss(&mut self, _: &mut Window, cx: &mut Context<Self>) -> DismissDecision {
        if self.busy || self.choosing {
            self.close(cx);
            DismissDecision::Pending
        } else {
            super::defer_first_launch_setup(cx);
            DismissDecision::Dismiss(true)
        }
    }

    fn fade_out_background(&self) -> bool {
        true
    }
}

impl Render for SetupWizard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.rendered_step != self.step || self.rendered_settings != self.show_settings {
            // Page changes invalidate deferred reveals and retained focus memos.
            for entry in self.focus_reveals.borrow().values() {
                *entry.previous.borrow_mut() = None;
            }
            self.rendered_settings = self.show_settings;
        }
        if self.rendered_step != self.step {
            self.content_scroll.set_offset(Default::default());
            self.license_scroll.set_offset(Default::default());
            self.rendered_step = self.step;
        }
        if self.rendered_error != self.error {
            if self.error.is_some() {
                self.content_scroll.set_offset(Default::default());
                if !self.busy && !self.choosing {
                    self.content_focus.focus(window, cx);
                }
            }
            self.rendered_error = self.error.clone();
        }
        let viewport = window.viewport_size();
        let width = (viewport.width - px(64.)).min(px(800.)).max(px(280.));
        let compact_height = viewport.height < px(400.);
        let compact_width = viewport.width < px(620.);
        let height = (viewport.height - px(100.)).min(px(560.)).max(px(120.));
        let busy = self.busy || self.choosing;
        let finishing = self.step == SetupStep::Installing
            && self
                .progress
                .lock()
                .is_ok_and(|progress| progress.finishing);
        let content = match self.step {
            SetupStep::Review => self.review(cx),
            SetupStep::Licenses => self.licenses(window, cx),
            SetupStep::Installing => self.installing(cx),
            SetupStep::Ready => self.ready(cx),
        };
        let next_label = match self.step {
            SetupStep::Review if self.busy => "Resolving components…",
            SetupStep::Review if self.plan.is_none() => "Refresh download summary",
            SetupStep::Review
                if self
                    .plan
                    .as_ref()
                    .is_some_and(|plan| plan.licenses.is_empty()) =>
            {
                "Set up"
            }
            SetupStep::Licenses => "Accept and download",
            SetupStep::Ready => "Finish setup",
            SetupStep::Installing if finishing => "Finishing…",
            SetupStep::Installing => "Installing…",
            SetupStep::Review => "Continue",
        };
        let next_disabled = !self.can_advance();
        h_flex().id("android-setup-wizard").debug_selector(|| "android-setup-wizard".into()).key_context("AndroidSetupWizard").tab_group()
            .track_focus(&self.focus_handle).w(width).h(height).elevation_3(cx).overflow_hidden()
            .on_key_down(cx.listener(|wizard, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" { wizard.close(cx); cx.stop_propagation(); }
                let modifiers = event.keystroke.modifiers;
                if event.keystroke.key == "tab" && !modifiers.control && !modifiers.alt && !modifiers.platform && !modifiers.function {
                    wizard.move_focus(modifiers.shift, window, cx);
                    window.prevent_default(); cx.stop_propagation(); cx.notify(); return;
                }
                let scroll = if wizard.license_focus.is_focused(window) {
                    Some(&wizard.license_scroll)
                } else if wizard.content_focus.is_focused(window) || wizard.focus_handle.is_focused(window) {
                    Some(&wizard.content_scroll)
                } else { None };
                if !modifiers.modified() && scroll.is_some_and(|handle| Self::scroll_key(handle, &event.keystroke.key)) {
                    window.prevent_default(); cx.stop_propagation(); cx.notify(); return;
                }
                if event.keystroke.key == "enter" && !event.keystroke.modifiers.modified()
                    && wizard.focus_handle.is_focused(window) && wizard.can_advance() && wizard.step != SetupStep::Licenses {
                    wizard.next(window, cx); window.prevent_default(); cx.stop_propagation();
                }
            }))
            .when(viewport.width >= px(760.) && !compact_height, |element| element.child(v_flex().w(px(160.)).h_full().flex_shrink_0().p_5().gap_6().bg(cx.theme().colors().background).border_r_1().border_color(cx.theme().colors().border)
                .child(v_flex().gap_2().child(Icon::new(IconName::Koda).size(IconSize::XLarge)).child(ui::Headline::new("Koda")).child(Label::new("Android development").size(LabelSize::Small).color(Color::Muted)))
                .child(v_flex().gap_3().children([(SetupStep::Review, "Setup"), (SetupStep::Licenses, "Licenses"), (SetupStep::Installing, "Download"), (SetupStep::Ready, "Ready")].into_iter().map(|(step, label)|
                    h_flex().gap_2().child(Icon::new(if self.step == step { IconName::ChevronRight } else { IconName::Dash }).size(IconSize::Small).color(if self.step == step { Color::Accent } else { Color::Muted })).child(Label::new(label).color(if self.step == step { Color::Default } else { Color::Muted })))))))
            .child(v_flex().flex_1().min_w_0().h_full()
            .child(v_flex().flex_shrink_0().px_6().pt_6().pb_2().when(compact_height, |element| element.p_2()).gap_1()
                .child(ui::Headline::new(if self.step == SetupStep::Review { if self.show_settings { "Customize setup" } else { "Welcome to Koda" } } else { self.step.title() }).size(ui::HeadlineSize::XLarge)))
            .child(div().id("android-setup-content-frame").flex_1().min_h_0().track_focus(&self.content_focus).role(gpui::Role::Pane).aria_label("Setup details").aria_description("Use arrow keys, Page Up, Page Down, Home and End to scroll setup details.").border_1().border_color(gpui::transparent_black()).focus_visible(|style| style.border_color(cx.theme().colors().border_focused))
                .child(v_flex().id("android-setup-content").size_full().overflow_y_scroll().track_scroll(&self.content_scroll).px_6().py_3().gap_3()
                .when_some(self.error.clone(), |element, error| element.child(v_flex().id("android-setup-error").debug_selector(|| "android-setup-error".into()).role(gpui::Role::Alert).aria_label("Setup failed").aria_description(error.clone()).p_3().gap_2().rounded_md().border_1().border_color(cx.theme().status().error.opacity(0.2)).bg(cx.theme().status().error.opacity(0.08))
                    .child(Self::text(error).text_color(cx.theme().status().error))
                    .child(Self::text("Your previous selection is preserved. Resolve the error, then retry."))
                    .when(self.step == SetupStep::Review, |element| element.child(div().debug_selector(|| "android-setup-retry-control".into()).child(self.focus_row("reveal-android-setup-retry-detection", setup_button("android-setup-retry-detection", "Retry detection", cx).disabled(busy).tab_index(0isize).on_click(cx.listener(|wizard, _, window, cx| wizard.detect(window, cx))), cx))))))
                .child(content))
                .custom_scrollbars(ui::Scrollbars::always_visible(ui::ScrollAxes::Vertical).tracked_scroll_handle(&self.content_scroll).tracked_entity(cx.entity_id()), window, cx))
            .child(h_flex().debug_selector(|| "android-setup-footer".into()).flex_shrink_0().p_4().when(compact_height, |element| element.p_2()).gap_2().flex_wrap().justify_between().border_t_1().border_color(cx.theme().colors().border)
                .child(div().debug_selector(|| "android-setup-cancel-control".into()).child(setup_button("android-setup-cancel", if self.close_requested && finishing { "Finishing…" } else if self.close_requested { "Cancelling…" } else if finishing { "Close when finished" } else if busy { "Cancel setup" } else { "Not now" }, cx)
                    .disabled(self.close_requested).tab_index(0isize)
                    .on_click(cx.listener(|wizard, _, _, cx| wizard.close(cx)))))
                .when(!compact_width, |element| element.child(div().flex_1()))
                .when(self.step == SetupStep::Licenses, |element| element.child(div().debug_selector(|| "android-setup-back-control".into()).child(setup_button("android-setup-back", "Back to summary", cx).disabled(busy).tab_index(0isize).on_click(cx.listener(|wizard, _, _, cx| wizard.back(cx))))))
                .child(div().debug_selector(|| "android-setup-next-control".into()).child(setup_button("android-setup-next", next_label, cx).style(ButtonStyle::Tinted(TintColor::Accent)).disabled(next_disabled).tab_index(0isize).on_click(cx.listener(|wizard, _, window, cx| wizard.next(window, cx)))))))
    }
}

fn setup_button(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    _cx: &App,
) -> Button {
    Button::new(id, label)
        .style(ButtonStyle::Subtle)
        .size(ButtonSize::Medium)
}

fn download_label(download: &provision::Download) -> String {
    if let Some(api) = download.label.strip_prefix("platforms;android-") {
        format!("Android SDK Platform {api}")
    } else if let Some(version) = download.label.strip_prefix("build-tools;") {
        format!("Android SDK Build Tools {version}")
    } else if download.label == "platform-tools" {
        format!("Android SDK Platform Tools {}", download.version)
    } else if download.label.contains(&download.version) {
        download.label.clone()
    } else {
        format!("{} {}", download.label, download.version)
    }
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.1} GiB", bytes as f64 / (1024. * 1024. * 1024.))
    } else if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024. * 1024.))
    } else {
        format!("{bytes} bytes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{KeyUpEvent, Keystroke, TestAppContext};
    use serde_json::json;

    async fn fixture(
        cx: &mut TestAppContext,
    ) -> (
        Arc<workspace::AppState>,
        Entity<Workspace>,
        Entity<AndroidPanel>,
    ) {
        let state = cx.update(workspace::AppState::test);
        let filesystem = project::FakeFs::new(cx.executor());
        filesystem.insert_tree("/android", json!({"app": {}})).await;
        let project = Project::test(filesystem, [Path::new("/android")], cx).await;
        let workspace = cx
            .add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx))
            .0;
        let panel = cx.new(|cx| AndroidPanel::new(workspace.downgrade(), project, cx));
        (state, workspace, panel)
    }

    fn plan(licenses: Vec<provision::License>) -> provision::SetupPlan {
        serde_json::from_value(json!({
            "id": "fixture-plan", "jdk_version": "21", "packages": [], "download_bytes": 0,
            "licenses": licenses, "installation_directory": "/managed/generation", "provenance": [],
            "jdk": "/managed/jdk", "sdk": null, "sdk_is_shared": false, "downloads": [],
            "supported_platform": "Test", "slot": "fixture", "options": provision::Options::default(),
            "artifacts": [], "environment_digest": "fixture", "recipe": "fixture", "sdk_destination": null, "sdk_packages": []
        })).expect("Wizard fixture plan")
    }

    fn license(id: &str) -> provision::License {
        provision::License {
            id: id.into(),
            text: "Terms that require explicit user consent.\n".repeat(100),
            sha256: format!("hash-{id}"),
            source: "https://developer.android.com/studio/terms".into(),
        }
    }

    fn press_key(cx: &mut gpui::VisualTestContext, key: &str) {
        // simulate_keystrokes dispatches only key-down; clickable controls
        // activate on key-up, so exercise a complete keyboard press here.
        let keystroke = Keystroke::parse(key).expect("Keyboard fixture key");
        cx.simulate_event(KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.simulate_next_frame(cx);
        });
        cx.run_until_parked();
        cx.simulate_event(KeyUpEvent { keystroke });
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.simulate_next_frame(cx);
        });
        cx.run_until_parked();
    }

    struct KeyboardFixture {
        wizard: Entity<SetupWizard>,
        background_focus: FocusHandle,
    }

    impl Render for KeyboardFixture {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .child(
                    setup_button("background-control", "Background control", cx)
                        .track_focus(&self.background_focus)
                        .tab_index(0isize),
                )
                .child(self.wizard.clone())
        }
    }

    #[gpui::test]
    async fn license_actions_stay_inside_narrow_and_short_setup_windows(cx: &mut TestAppContext) {
        let (_state, _workspace, panel) = fixture(cx).await;
        let (wizard, cx) = cx.add_window_view(|window, cx| SetupWizard::new(panel, window, cx));
        wizard.update(cx, |wizard, cx| {
            wizard.plan = Some(plan(vec![license("android-sdk-license")]));
            wizard.step = SetupStep::Licenses;
            cx.notify();
        });
        for (width, height) in [(360., 600.), (480., 400.), (360., 240.)] {
            cx.simulate_resize(gpui::size(px(width), px(height)));
            cx.run_until_parked();
            let bounds = cx
                .debug_bounds("android-setup-wizard")
                .expect("Setup bounds");
            for selector in [
                "android-setup-cancel-control",
                "android-setup-back-control",
                "android-setup-next-control",
            ] {
                let action = cx.debug_bounds(selector).expect("Setup action");
                assert!(
                    action.left() >= bounds.left() && action.right() <= bounds.right(),
                    "{selector} fits width {width}"
                );
                assert!(
                    action.top() >= bounds.top() && action.bottom() <= bounds.bottom(),
                    "{selector} fits height {height}"
                );
            }
        }
    }

    #[gpui::test]
    async fn licenses_require_individual_consent_and_changed_text_invalidates_it(
        cx: &mut TestAppContext,
    ) {
        let (_state, _workspace, panel) = fixture(cx).await;
        let (wizard, cx) = cx.add_window_view(|window, cx| SetupWizard::new(panel, window, cx));
        wizard.update_in(cx, |wizard, window, cx| {
            wizard.plan = Some(plan(vec![license("android-sdk-license")]));
            wizard.step = SetupStep::Licenses;
            wizard.focus_handle.focus(window, cx);
            cx.notify();
        });
        cx.run_until_parked();
        press_key(cx, "enter");
        wizard.read_with(cx, |wizard, _| {
            assert!(wizard.acceptances.is_empty());
            assert!(!wizard.can_advance());
        });
        let accept = cx
            .debug_bounds("android-setup-license-accept")
            .expect("License consent control");
        cx.simulate_click(accept.center(), Default::default());
        wizard.read_with(cx, |wizard, _| assert!(wizard.can_advance()));
        for key in ["space", "enter"] {
            let before = wizard.read_with(cx, |wizard, _| wizard.acceptances.len());
            let keystroke = Keystroke::parse(key).expect("Activation key");
            cx.simulate_event(KeyDownEvent {
                keystroke: keystroke.clone(),
                is_held: false,
                prefer_character_input: false,
            });
            wizard.update(cx, |_, cx| cx.notify());
            cx.run_until_parked();
            wizard.read_with(cx, |wizard, _| {
                assert_eq!(wizard.acceptances.len(), before);
                assert_eq!(wizard.step, SetupStep::Licenses);
            });
            cx.simulate_event(KeyUpEvent { keystroke });
            cx.run_until_parked();
            wizard.read_with(cx, |wizard, _| {
                assert_eq!(wizard.acceptances.len(), 1 - before);
                assert_eq!(wizard.step, SetupStep::Licenses);
            });
        }
        wizard.update(cx, |wizard, cx| {
            let license = wizard
                .plan
                .as_mut()
                .expect("Plan")
                .licenses
                .get_mut(0)
                .expect("SDK license");
            license.sha256 = "changed-terms".into();
            cx.notify();
        });
        wizard.read_with(cx, |wizard, _| assert!(!wizard.can_advance()));
    }

    #[gpui::test]
    async fn focused_child_buttons_activate_on_release_without_advancing_the_wizard(
        cx: &mut TestAppContext,
    ) {
        let (_state, _workspace, panel) = fixture(cx).await;
        let (wizard, cx) = cx.add_window_view(|window, cx| SetupWizard::new(panel, window, cx));
        cx.run_until_parked();
        // ButtonLike preserves the existing focus on mouse-down. Use actual
        // tab input to reach the details pane and Change settings.
        wizard.update_in(cx, |wizard, window, cx| {
            wizard.focus_handle.focus(window, cx);
        });
        cx.run_until_parked();
        for _ in 0..2 {
            press_key(cx, "tab");
            cx.run_until_parked();
        }
        cx.run_until_parked();
        let keystroke = Keystroke::parse("enter").expect("Enter");
        cx.simulate_event(KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        wizard.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        wizard.read_with(cx, |wizard, _| {
            assert!(!wizard.show_settings);
            assert_eq!(wizard.step, SetupStep::Review);
        });
        cx.simulate_event(KeyUpEvent { keystroke });
        wizard.read_with(cx, |wizard, _| {
            assert!(wizard.show_settings);
            assert_eq!(wizard.step, SetupStep::Review);
        });
        wizard.update(cx, |wizard, cx| {
            wizard.step = SetupStep::Licenses;
            wizard.plan = Some(plan(vec![license("android-sdk-license")]));
            cx.notify();
        });
        cx.run_until_parked();
        wizard.update_in(cx, |wizard, window, cx| {
            wizard.focus_handle.focus(window, cx);
        });
        cx.run_until_parked();
        // Download is disabled until consent, so the last enabled control is Back.
        press_key(cx, "shift-tab");
        cx.run_until_parked();
        let keystroke = Keystroke::parse("enter").expect("Enter");
        cx.simulate_event(KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        wizard.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        wizard.read_with(cx, |wizard, _| assert_eq!(wizard.step, SetupStep::Licenses));
        cx.simulate_event(KeyUpEvent { keystroke });
        wizard.read_with(cx, |wizard, _| assert_eq!(wizard.step, SetupStep::Review));
    }

    #[gpui::test]
    async fn unsupported_download_platform_can_reuse_existing_tools_and_unselect_sdk(
        cx: &mut TestAppContext,
    ) {
        let (_state, _workspace, panel) = fixture(cx).await;
        let (wizard, cx) = cx.add_window_view(|window, cx| SetupWizard::new(panel, window, cx));
        wizard.update(cx, |wizard, cx| {
            wizard.step = SetupStep::Review;
            wizard.show_settings = true;
            wizard.jdk = Some("/existing/jdk".into());
            wizard.sdk = Some("/existing/sdk".into());
            wizard.api_level = 34;
            wizard.discovery.as_mut().expect("Discovery").supported = false;
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("android-setup-cli").is_none());
        wizard.read_with(cx, |wizard, _| assert!(wizard.can_advance()));
        let reuse_jdk = cx
            .debug_bounds("android-setup-reuse-jdk")
            .expect("Reuse JDK");
        let viewport = wizard.read_with(cx, |wizard, _| wizard.content_scroll.bounds());
        cx.simulate_event(gpui::ScrollWheelEvent {
            position: viewport.center(),
            delta: gpui::ScrollDelta::Pixels(gpui::point(
                px(0.),
                (viewport.bottom() - reuse_jdk.bottom() - px(24.)).min(px(0.)),
            )),
            ..Default::default()
        });
        cx.run_until_parked();
        let reuse_jdk = cx
            .debug_bounds("android-setup-reuse-jdk")
            .expect("Visible JDK reuse control");
        assert!(reuse_jdk.bottom() <= viewport.bottom());
        cx.simulate_click(reuse_jdk.center(), Default::default());
        wizard.read_with(cx, |wizard, _| assert!(!wizard.can_advance()));
        cx.simulate_click(reuse_jdk.center(), Default::default());
        wizard.update(cx, |wizard, cx| {
            wizard.sdk = None;
            cx.notify();
        });
        cx.run_until_parked();
        wizard.read_with(cx, |wizard, _| assert!(!wizard.can_advance()));
        let sdk = cx.debug_bounds("android-setup-sdk").expect("SDK component");
        let viewport = wizard.read_with(cx, |wizard, _| wizard.content_scroll.bounds());
        cx.simulate_event(gpui::ScrollWheelEvent {
            position: viewport.center(),
            delta: gpui::ScrollDelta::Pixels(gpui::point(
                px(0.),
                (viewport.bottom() - sdk.bottom() - px(24.)).min(px(0.)),
            )),
            ..Default::default()
        });
        cx.run_until_parked();
        let sdk = cx
            .debug_bounds("android-setup-sdk")
            .expect("Visible SDK component");
        assert!(sdk.bottom() <= viewport.bottom());
        cx.simulate_click(sdk.center(), Default::default());
        wizard.read_with(cx, |wizard, _| {
            assert!(!wizard.install_sdk);
            assert!(wizard.can_advance());
            assert_eq!(wizard.api_level, 34);
        });
    }

    #[gpui::test]
    async fn download_summary_goes_directly_to_sdk_terms_and_folder_changes_require_a_new_review(
        cx: &mut TestAppContext,
    ) {
        let (_state, _workspace, panel) = fixture(cx).await;
        let (wizard, cx) = cx.add_window_view(|window, cx| SetupWizard::new(panel, window, cx));
        wizard.update_in(cx, |wizard, window, cx| {
            let mut downloads = plan(vec![license("android-sdk-license")]);
            downloads.sdk = Some("/shared/Android/Sdk".into());
            downloads.sdk_is_shared = true;
            downloads.download_bytes = 300 * 1024 * 1024;
            downloads.downloads = vec![
                provision::Download {
                    label: "Temurin JDK".into(),
                    version: "21".into(),
                    publisher: "Eclipse Adoptium".into(),
                    url: "https://github.com/adoptium/temurin21-binaries/releases/download/fixture/jdk.tar.gz".into(),
                    bytes: 200 * 1024 * 1024,
                },
                provision::Download {
                    label: "Android SDK Platform".into(),
                    version: "36".into(),
                    publisher: "Google".into(),
                    url: "https://dl.google.com/android/repository/fixture-platform.zip".into(),
                    bytes: 100 * 1024 * 1024,
                },
            ];
            wizard.plan = Some(downloads);
            wizard.sdk_destination = Some("/shared/Android/Sdk".into());
            wizard.focus_handle.focus(window, cx);
            cx.notify();
        });
        cx.simulate_resize(gpui::size(px(900.), px(700.)));
        cx.run_until_parked();
        let summary = cx
            .debug_bounds("android-setup-download-summary")
            .expect("Download summary");
        let footer = cx
            .debug_bounds("android-setup-footer")
            .expect("Visible actions");
        assert!(summary.bottom() <= footer.top());
        assert!(
            cx.debug_bounds("android-setup-sdk").is_none(),
            "Advanced choices start collapsed"
        );
        assert!(cx.debug_bounds("android-setup-runtime-details").is_none());
        assert!(
            cx.debug_bounds("android-setup-summary-sdk-folder-control")
                .is_none(),
            "Folder controls belong to Customize"
        );
        let details = cx
            .debug_bounds("android-setup-summary-details-control")
            .expect("Details disclosure");
        cx.simulate_click(details.center(), Default::default());
        cx.run_until_parked();
        assert!(cx.debug_bounds("android-setup-runtime-details").is_some());
        cx.simulate_click(details.center(), Default::default());
        cx.run_until_parked();
        assert!(cx.debug_bounds("android-setup-runtime-details").is_none());
        wizard.update_in(cx, |wizard, window, cx| {
            wizard.focus_handle.focus(window, cx)
        });
        press_key(cx, "enter");
        wizard.read_with(cx, |wizard, _| {
            assert_eq!(wizard.step, SetupStep::Licenses);
            assert!(wizard.acceptances.is_empty());
            assert_eq!(
                wizard.plan.as_ref().expect("Reviewed plan").licenses.len(),
                1
            );
        });
        wizard.update(cx, |wizard, cx| wizard.back(cx));
        cx.run_until_parked();
        let customize = cx
            .debug_bounds("android-setup-change-control")
            .expect("Customize action");
        cx.simulate_click(customize.center(), Default::default());
        cx.run_until_parked();
        wizard.read_with(cx, |wizard, _| assert!(wizard.show_settings));
        let choose = cx
            .debug_bounds("android-setup-summary-sdk-folder-control")
            .expect("Change SDK folder button");
        let viewport = wizard.read_with(cx, |wizard, _| wizard.content_scroll.bounds());
        cx.simulate_event(gpui::ScrollWheelEvent {
            position: viewport.center(),
            delta: gpui::ScrollDelta::Pixels(gpui::point(
                px(0.),
                (viewport.bottom() - choose.bottom() - px(24.)).min(px(0.)),
            )),
            ..Default::default()
        });
        cx.run_until_parked();
        let choose = cx
            .debug_bounds("android-setup-summary-sdk-folder-control")
            .expect("Visible folder action");
        assert!(choose.bottom() <= viewport.bottom());
        cx.simulate_click(choose.center(), Default::default());
        wizard.read_with(cx, |wizard, _| assert!(wizard.choosing));
        cx.simulate_path_prompt_response(|_| Some(vec![PathBuf::from("/another/Android/Sdk")]));
        cx.run_until_parked();
        wizard.read_with(cx, |wizard, _| {
            assert_eq!(
                wizard.sdk_destination.as_deref(),
                Some(Path::new("/another/Android/Sdk"))
            );
            assert!(!wizard.reuse_sdk);
            assert!(
                wizard.plan.is_none(),
                "A changed destination requires a fresh summary before downloading"
            );
            assert!(wizard.acceptances.is_empty());
            assert_eq!(wizard.step, SetupStep::Review);
        });
    }

    #[gpui::test]
    async fn folder_choice_cancellation_preserves_error_and_success_clears_it(
        cx: &mut TestAppContext,
    ) {
        let (_state, _workspace, panel) = fixture(cx).await;
        let (wizard, cx) =
            cx.add_window_view(|window, cx| SetupWizard::new(panel.clone(), window, cx));
        wizard.update_in(cx, |wizard, window, cx| {
            wizard.step = SetupStep::Review;
            wizard.error = Some("Previous invalid JDK".into());
            wizard.choose_path(Dependency::Jdk, window, cx);
        });
        assert!(panel.read_with(cx, |panel, _| panel.tool_setup.choosing));
        cx.simulate_path_prompt_response(|_| None);
        cx.run_until_parked();
        wizard.read_with(cx, |wizard, _| {
            assert_eq!(wizard.error.as_deref(), Some("Previous invalid JDK"));
            assert!(!wizard.choosing);
        });
        wizard.update_in(cx, |wizard, window, cx| {
            wizard.choose_path(Dependency::Jdk, window, cx)
        });
        cx.simulate_path_prompt_response(|_| Some(vec![PathBuf::from("/selected/jdk")]));
        cx.run_until_parked();
        wizard.read_with(cx, |wizard, _| {
            assert_eq!(wizard.jdk.as_deref(), Some(Path::new("/selected/jdk")));
            assert!(wizard.error.is_none());
        });
        assert!(!panel.read_with(cx, |panel, _| panel.tool_setup.choosing));
    }

    #[gpui::test]
    async fn busy_close_waits_for_folder_picker_and_stale_cleanup_cannot_unlock_new_operation(
        cx: &mut TestAppContext,
    ) {
        let (_state, _workspace, panel) = fixture(cx).await;
        let (wizard, cx) =
            cx.add_window_view(|window, cx| SetupWizard::new(panel.clone(), window, cx));
        wizard.update_in(cx, |wizard, window, cx| {
            wizard.choose_path(Dependency::Jdk, window, cx);
            assert!(matches!(
                wizard.on_before_dismiss(window, cx),
                DismissDecision::Pending
            ));
            assert!(wizard.cancel.load(Ordering::Acquire));
            assert!(!wizard.can_advance());
        });
        cx.simulate_path_prompt_response(|_| None);
        cx.run_until_parked();
        assert!(!panel.read_with(cx, |panel, _| panel.tool_setup.choosing));
        let obsolete = Arc::new(AtomicBool::new(false));
        let current = Arc::new(AtomicBool::new(false));
        panel.update(cx, |panel, _| {
            panel.tool_setup.choosing = true;
            panel.tool_setup.native_cancel = Some(current.clone());
        });
        cx.update(|_, cx| SetupWizard::release_native_job(&panel, &obsolete, cx));
        assert!(panel.read_with(cx, |panel, _| panel.tool_setup.choosing));
        cx.update(|_, cx| SetupWizard::release_native_job(&panel, &current, cx));
        assert!(!panel.read_with(cx, |panel, _| panel.tool_setup.choosing));
    }

    #[gpui::test]
    async fn setup_failures_expose_actionable_error_before_long_content_and_allow_keyboard_retry(
        cx: &mut TestAppContext,
    ) {
        let (_state, _workspace, panel) = fixture(cx).await;
        panel.update(cx, |panel, cx| {
            panel.tool_setup.expanded = true;
            panel.tool_setup.lines = (0..20)
                .map(|index| {
                    format!(
                        "Tool {index}: Runtime integrity check failed. Choose Install / repair."
                    )
                })
                .collect();
            cx.notify();
        });
        let (wizard, cx) = cx.add_window_view(|window, cx| SetupWizard::new(panel, window, cx));
        wizard.update(cx, |wizard, cx| {
            let mut reviewed_plan = plan(Vec::new());
            reviewed_plan.provenance = (0..20)
                .map(|index| {
                    format!(
                        "Reviewed package {index}: Publisher checksum and destination verified."
                    )
                })
                .collect();
            wizard.plan = Some(reviewed_plan);
            wizard.show_settings = true;
            wizard.show_details = true;
            cx.notify();
        });
        cx.simulate_resize(gpui::size(px(900.), px(700.)));
        cx.run_until_parked();
        for planned in [true, false] {
            wizard.update(cx, |wizard, cx| {
                wizard.step = SetupStep::Installing;
                wizard.error = None;
                cx.notify();
            });
            cx.run_until_parked();
            wizard.update(cx, |wizard, cx| {
                wizard.step = SetupStep::Review;
                if !planned { wizard.plan = None; }
                wizard.error = Some("Runtime integrity check failed: sdk/platform-tools/source.properties. Choose Install / repair.".into());
                cx.notify();
            });
            cx.run_until_parked();
            let error = cx
                .debug_bounds("android-setup-error")
                .expect("Visible integrity error");
            let viewport = wizard.read_with(cx, |wizard, _| {
                assert!(wizard.content_scroll.max_offset().y > px(0.));
                assert_eq!(wizard.content_scroll.offset(), Default::default());
                wizard.content_scroll.bounds()
            });
            assert!(error.top() >= viewport.top());
            assert!(
                error.bottom() <= viewport.bottom(),
                "The actionable error is visible with a reviewed plan: {planned}"
            );
            assert!(wizard.update_in(cx, |wizard, window, _| {
                wizard.content_focus.is_focused(window)
            }));

            press_key(cx, "end");
            wizard.read_with(cx, |wizard, _| {
                assert!(wizard.content_scroll.offset().y < px(0.))
            });
            press_key(cx, "home");
            let error = cx
                .debug_bounds("android-setup-error")
                .expect("Error after keyboard Home");
            assert!(error.top() >= viewport.top());
            assert!(error.bottom() <= viewport.bottom());

            press_key(cx, "end");
            wizard.update_in(cx, |wizard, window, cx| {
                let repeated_error = wizard.error.clone();
                wizard.detect(window, cx);
                assert!(wizard.error.is_none());
                wizard.error = repeated_error;
                wizard.focus_handle.focus(window, cx);
                cx.notify();
            });
            cx.run_until_parked();
            let error = cx
                .debug_bounds("android-setup-error")
                .expect("Repeated error without an intermediate frame");
            assert!(error.top() >= viewport.top());
            assert!(error.bottom() <= viewport.bottom());
            assert!(wizard.update_in(cx, |wizard, window, _| {
                wizard.content_focus.is_focused(window)
            }));

            press_key(cx, "end");
            wizard.update(cx, |wizard, cx| {
                wizard.error = Some("Download failed: insufficient disk space. Free storage and retry the reviewed installation.".into());
                cx.notify();
            });
            cx.run_until_parked();
            let error = cx
                .debug_bounds("android-setup-error")
                .expect("New error on the same page");
            assert!(error.top() >= viewport.top());
            assert!(
                error.bottom() <= viewport.bottom(),
                "A new same-page error returns to view"
            );
            wizard.read_with(cx, |wizard, _| {
                assert_eq!(wizard.content_scroll.offset(), Default::default())
            });
        }

        wizard.update(cx, |wizard, cx| {
            wizard.step = SetupStep::Review;
            wizard.error = Some("Dependency detection failed: chosen Java folder is unavailable. Choose another folder or retry detection.".into());
            cx.notify();
        });
        cx.run_until_parked();
        let retry = cx
            .debug_bounds("android-setup-retry-control")
            .expect("Retry is visible beside the error");
        let viewport = wizard.read_with(cx, |wizard, _| wizard.content_scroll.bounds());
        assert!(retry.top() >= viewport.top());
        assert!(retry.bottom() <= viewport.bottom());
        press_key(cx, "tab");
        press_key(cx, "enter");
        wizard.read_with(cx, |wizard, _| {
            assert!(
                wizard.error.is_none(),
                "Keyboard retry invokes dependency detection"
            );
            assert!(wizard.discovery.is_some());
            assert_eq!(wizard.step, SetupStep::Review);
        });
        assert!(cx.debug_bounds("android-setup-error").is_none());

        wizard.update_in(cx, |wizard, window, cx| {
            wizard.panel.update(cx, |panel, _| panel.running = true);
            wizard.step = SetupStep::Review;
            wizard.focus_handle.focus(window, cx);
            cx.notify();
        });
        cx.run_until_parked();
        press_key(cx, "enter");
        let previous_error = wizard.read_with(cx, |wizard, _| wizard.error.clone());
        assert!(
            previous_error
                .as_ref()
                .is_some_and(|error| error.contains("Finish the current Android operation"))
        );
        press_key(cx, "end");
        wizard.read_with(cx, |wizard, _| {
            assert!(wizard.content_scroll.offset().y < px(0.))
        });
        wizard.update_in(cx, |wizard, window, cx| {
            wizard.focus_handle.focus(window, cx)
        });
        cx.run_until_parked();
        press_key(cx, "enter");
        let error = cx
            .debug_bounds("android-setup-error")
            .expect("Repeated blocked operation");
        let viewport = wizard.read_with(cx, |wizard, _| {
            assert_eq!(wizard.error, previous_error);
            wizard.content_scroll.bounds()
        });
        assert!(error.top() >= viewport.top());
        assert!(error.bottom() <= viewport.bottom());
        wizard.update(cx, |wizard, cx| {
            wizard.panel.update(cx, |panel, _| panel.running = false)
        });
    }

    #[gpui::test]
    async fn long_license_keeps_navigation_footer_outside_scroll_content(cx: &mut TestAppContext) {
        let (_state, _workspace, panel) = fixture(cx).await;
        let (wizard, cx) = cx.add_window_view(|window, cx| SetupWizard::new(panel, window, cx));
        wizard.update(cx, |wizard, cx| {
            wizard.plan = Some(plan(vec![license("android-sdk-license")]));
            wizard.step = SetupStep::Licenses;
            cx.notify();
        });
        cx.simulate_resize(gpui::size(px(900.), px(700.)));
        cx.run_until_parked();
        let modal = cx.debug_bounds("android-setup-wizard").expect("Wizard");
        let footer = cx
            .debug_bounds("android-setup-footer")
            .expect("Fixed navigation footer");
        let license = cx
            .debug_bounds("android-setup-license-text")
            .expect("Scrollable license");
        assert!(footer.bottom() <= modal.bottom());
        assert!(license.bottom() <= footer.top());
        wizard.read_with(cx, |wizard, _| assert!(!wizard.can_advance()));
    }

    #[gpui::test]
    async fn license_wheel_and_thumb_scroll_independently_and_switching_terms_resets_to_top(
        cx: &mut TestAppContext,
    ) {
        let (_state, _workspace, panel) = fixture(cx).await;
        let (wizard, cx) = cx.add_window_view(|window, cx| SetupWizard::new(panel, window, cx));
        wizard.update(cx, |wizard, cx| {
            wizard.plan = Some(plan(vec![license("android-sdk-license")]));
            wizard.step = SetupStep::Licenses;
            cx.notify();
        });
        cx.simulate_resize(gpui::size(px(900.), px(700.)));
        cx.run_until_parked();
        let license_bounds = cx
            .debug_bounds("android-setup-license-text")
            .expect("Scrollable terms");
        let outer_offset = wizard.read_with(cx, |wizard, _| {
            assert!(wizard.license_scroll.max_offset().y > px(0.));
            assert_eq!(wizard.license_scroll.bounds(), license_bounds);
            wizard.content_scroll.offset()
        });
        cx.simulate_event(gpui::ScrollWheelEvent {
            position: license_bounds.center(),
            delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), px(-120.))),
            ..Default::default()
        });
        cx.run_until_parked();
        wizard.read_with(cx, |wizard, _| {
            assert!(wizard.license_scroll.offset().y < px(0.));
            assert_eq!(wizard.content_scroll.offset(), outer_offset);
        });
        let sdk_group = cx
            .debug_bounds("android-license-group-0")
            .expect("SDK license group");
        cx.simulate_click(sdk_group.center(), Default::default());
        cx.run_until_parked();
        wizard.read_with(cx, |wizard, _| {
            assert_eq!(wizard.selected_license, 0);
            assert_eq!(wizard.license_scroll.offset(), Default::default());
            assert_eq!(wizard.content_scroll.offset(), outer_offset);
            assert!(wizard.acceptances.is_empty());
        });
        let license_bounds = cx
            .debug_bounds("android-setup-license-frame")
            .expect("SDK terms scrollbar frame");
        let thumb = gpui::point(
            license_bounds.right() - px(7.),
            license_bounds.top() + px(10.),
        );
        let destination = gpui::point(thumb.x, license_bounds.center().y);
        cx.simulate_mouse_down(thumb, gpui::MouseButton::Left, Default::default());
        cx.simulate_mouse_move(
            destination,
            Some(gpui::MouseButton::Left),
            Default::default(),
        );
        cx.simulate_mouse_up(destination, gpui::MouseButton::Left, Default::default());
        cx.run_until_parked();
        wizard.read_with(cx, |wizard, _| {
            assert!(
                wizard.license_scroll.offset().y < px(-100.),
                "Visible scrollbar thumb responds to dragging"
            );
            assert_eq!(wizard.content_scroll.offset(), outer_offset);
        });
    }

    #[gpui::test]
    async fn tab_reveals_narrow_license_consent_and_customize_options(cx: &mut TestAppContext) {
        let (_state, _workspace, panel) = fixture(cx).await;
        let (wizard, cx) = cx.add_window_view(|window, cx| SetupWizard::new(panel, window, cx));
        wizard.update_in(cx, |wizard, window, cx| {
            wizard.plan = Some(plan(vec![license("android-sdk-license")]));
            wizard.step = SetupStep::Licenses;
            wizard.focus_handle.focus(window, cx);
            cx.notify();
        });
        cx.simulate_resize(gpui::size(px(360.), px(600.)));
        cx.run_until_parked();
        for _ in 0..12 {
            press_key(cx, "tab");
            cx.run_until_parked();
            if wizard.update_in(cx, |wizard, window, cx| {
                wizard
                    .focus_reveals
                    .borrow()
                    .get("android-setup-license-accept")
                    .is_some_and(|entry| entry.focus.contains_focused(window, cx))
            }) {
                break;
            }
        }
        assert!(wizard.update_in(cx, |wizard, window, cx| {
            wizard
                .focus_reveals
                .borrow()
                .get("android-setup-license-accept")
                .is_some_and(|entry| entry.focus.contains_focused(window, cx))
        }));
        let consent = cx
            .debug_bounds("android-setup-license-accept")
            .expect("Consent");
        let viewport = wizard.read_with(cx, |wizard, _| wizard.content_scroll.bounds());
        assert!(
            consent.top() >= viewport.top() && consent.bottom() <= viewport.bottom(),
            "Consent visible before activation: {consent:?} in {viewport:?}"
        );
        wizard.read_with(cx, |wizard, _| assert!(wizard.acceptances.is_empty()));
        press_key(cx, "space");
        cx.run_until_parked();
        wizard.read_with(cx, |wizard, _| {
            assert_eq!(wizard.acceptances.len(), 1);
            assert_eq!(wizard.step, SetupStep::Licenses);
            assert!(wizard.can_advance());
        });
        // Redraws preserve manual scrolling while the same control remains focused.
        wizard.update(cx, |wizard, cx| {
            wizard.content_scroll.set_offset(Default::default());
            cx.notify();
        });
        cx.run_until_parked();
        wizard.read_with(cx, |wizard, _| {
            assert_eq!(wizard.content_scroll.offset(), Default::default())
        });
        wizard.update_in(cx, |wizard, window, cx| {
            wizard.step = SetupStep::Review;
            wizard.show_settings = true;
            wizard.focus_handle.focus(window, cx);
            cx.notify();
        });
        cx.run_until_parked();
        for _ in 0..16 {
            press_key(cx, "tab");
            cx.run_until_parked();
            if wizard.update_in(cx, |wizard, window, cx| {
                wizard
                    .focus_reveals
                    .borrow()
                    .get("android-setup-offline")
                    .is_some_and(|entry| entry.focus.contains_focused(window, cx))
            }) {
                break;
            }
        }
        assert!(wizard.update_in(cx, |wizard, window, cx| {
            wizard
                .focus_reveals
                .borrow()
                .get("android-setup-offline")
                .is_some_and(|entry| entry.focus.contains_focused(window, cx))
        }));
        let option = cx
            .debug_bounds("android-setup-offline")
            .expect("Offline option");
        let viewport = wizard.read_with(cx, |wizard, _| wizard.content_scroll.bounds());
        assert!(
            option.top() >= viewport.top() && option.bottom() <= viewport.bottom(),
            "Focused Customize option visible"
        );
    }

    #[gpui::test]
    async fn keyboard_reads_complete_terms_scrolls_details_and_keeps_focus_inside_setup(
        cx: &mut TestAppContext,
    ) {
        let (_state, _workspace, panel) = fixture(cx).await;
        let (fixture, cx) = cx.add_window_view(|window, cx| KeyboardFixture {
            wizard: cx.new(|cx| SetupWizard::new(panel, window, cx)),
            background_focus: cx.focus_handle().tab_index(0).tab_stop(true),
        });
        let wizard = fixture.read_with(cx, |fixture, _| fixture.wizard.clone());
        let background_focus = fixture.read_with(cx, |fixture, _| fixture.background_focus.clone());
        wizard.update_in(cx, |wizard, window, cx| {
            wizard.plan = Some(plan(vec![license("android-sdk-license")]));
            wizard.step = SetupStep::Licenses;
            wizard.focus_handle.focus(window, cx);
            cx.notify();
        });
        cx.simulate_resize(gpui::size(px(900.), px(700.)));
        cx.run_until_parked();
        for _ in 0..8 {
            press_key(cx, "tab");
            cx.run_until_parked();
            if wizard.update_in(cx, |wizard, window, _| {
                wizard.license_focus.is_focused(window)
            }) {
                break;
            }
        }
        assert!(
            wizard.update_in(cx, |wizard, window, _| wizard
                .license_focus
                .is_focused(window)),
            "Tab reaches the license document"
        );
        let outer_offset = wizard.read_with(cx, |wizard, _| wizard.content_scroll.offset());
        press_key(cx, "pagedown");
        cx.run_until_parked();
        let after_page = wizard.read_with(cx, |wizard, _| wizard.license_scroll.offset().y);
        assert!(after_page < px(0.));
        press_key(cx, "down");
        cx.run_until_parked();
        wizard.read_with(cx, |wizard, _| {
            assert!(wizard.license_scroll.offset().y < after_page)
        });
        press_key(cx, "end");
        cx.run_until_parked();
        let viewport = cx
            .debug_bounds("android-setup-license-text")
            .expect("License viewport");
        let document = cx
            .debug_bounds("android-setup-license-document")
            .expect("Complete terms");
        assert!(document.bottom() <= viewport.bottom());
        assert!(
            document.bottom() > viewport.top(),
            "End reveals the final lines"
        );
        press_key(cx, "home");
        cx.run_until_parked();
        let document = cx
            .debug_bounds("android-setup-license-document")
            .expect("Beginning of terms");
        assert!(document.top() >= viewport.top());
        assert!(document.top() < viewport.top() + px(24.));
        press_key(cx, "end");
        cx.run_until_parked();
        press_key(cx, "shift-tab");
        cx.run_until_parked();
        wizard.update_in(cx, |wizard, window, cx| {
            assert!(wizard.license_group_focus.get(0).is_some_and(|focus| focus.is_focused(window)), "Shift Tab from terms focuses SDK group; current focus {:?}, groups {:?}, document {:?}, details {:?}", window.focused(cx), wizard.license_group_focus, wizard.license_focus, wizard.content_focus);
        });
        press_key(cx, "enter");
        cx.run_until_parked();
        wizard.read_with(cx, |wizard, _| {
            assert_eq!(wizard.selected_license, 0);
            assert_eq!(wizard.license_scroll.offset(), Default::default());
            assert_eq!(wizard.content_scroll.offset(), outer_offset);
            assert!(wizard.acceptances.is_empty());
        });
        for key in ["tab", "shift-tab"] {
            for _ in 0..12 {
                press_key(cx, key);
                cx.run_until_parked();
                wizard.update_in(cx, |wizard, window, cx| {
                    assert!(wizard.focus_handle.contains_focused(window, cx));
                    assert!(!background_focus.is_focused(window));
                });
            }
        }
        wizard.update_in(cx, |wizard, window, cx| {
            wizard.step = SetupStep::Review;
            wizard.show_settings = true;
            wizard.focus_handle.focus(window, cx);
            cx.notify();
        });
        cx.run_until_parked();
        wizard.read_with(cx, |wizard, _| {
            assert!(wizard.content_scroll.max_offset().y > px(0.))
        });
        press_key(cx, "pagedown");
        cx.run_until_parked();
        wizard.read_with(cx, |wizard, _| {
            assert!(wizard.content_scroll.offset().y < px(0.))
        });
        press_key(cx, "end");
        cx.run_until_parked();
        let offline = cx
            .debug_bounds("android-setup-offline")
            .expect("Offscreen offline option");
        let details = wizard.read_with(cx, |wizard, _| wizard.content_scroll.bounds());
        assert!(offline.top() >= details.top());
        assert!(
            offline.bottom() <= details.bottom(),
            "End exposes the lower component options"
        );
        wizard.read_with(cx, |wizard, _| assert!(wizard.acceptances.is_empty()));
    }
}

impl AndroidPanel {
    pub(super) fn refresh_tool_setup(&mut self, cx: &mut Context<Self>) {
        self.tool_setup.check_generation = self.tool_setup.check_generation.saturating_add(1);
        let root = self.root.clone().or_else(|| self.auto_sync_candidate(cx));
        #[cfg(test)]
        {
            self.tool_setup.bootstrap_root = root;
            // Project model tests must not probe or launch the host's Android tools.
            self.tool_setup.bootstrap_ready = Some(true);
            self.tool_setup.checking = None;
            cx.notify();
        }
        #[cfg(not(test))]
        {
            self.tool_setup.bootstrap_root = root.clone();
            self.tool_setup.bootstrap_ready = None;
            let generation = self.tool_setup.check_generation;
            let discovery_root = root.clone();
            self.tool_setup.checking = Some(cx.spawn(async move |panel, cx| {
                let (mut lines, discovery) = cx
                    .background_spawn(async move {
                        (
                            managed::status(),
                            provision::discover(discovery_root.as_deref()),
                        )
                    })
                    .await;
                let ready = match discovery {
                    Ok(discovery) => discovery.jdk.is_some() && discovery.sdk.is_some(),
                    Err(error) => {
                        lines.push(format!("Dependency detection failed: {error:#}"));
                        false
                    }
                };
                panel
                    .update(cx, |panel, cx| {
                        let current_root =
                            panel.root.clone().or_else(|| panel.auto_sync_candidate(cx));
                        if panel.tool_setup.check_generation != generation || current_root != root {
                            return;
                        }
                        panel.tool_setup.lines = lines;
                        panel.tool_setup.bootstrap_ready = Some(ready);
                        panel.tool_setup.checking = None;
                        cx.notify();
                    })
                    .log_err();
            }));
            cx.notify();
        }
    }

    fn choose_dependency(
        &mut self,
        dependency: Dependency,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.running || self.syncing || self.tool_setup.choosing {
            return;
        }
        self.tool_setup.choosing = true;
        let selected = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(
                match dependency {
                    Dependency::Sdk => "Choose Android SDK (contains platform-tools)",
                    Dependency::Jdk => {
                        "Choose a full JDK 21 home (contains bin/java and bin/javac)"
                    }
                }
                .into(),
            ),
        });
        cx.spawn_in(window, async move |panel, cx| {
            let result = async {
                let paths = selected.await??;
                if let Some(path) = paths.and_then(|paths| paths.into_iter().next()) {
                    cx.background_spawn(async move { managed::save_dependency(dependency, &path) })
                        .await?;
                    return anyhow::Ok(true);
                }
                anyhow::Ok(false)
            }
            .await;
            panel
                .update_in(cx, |panel, window, cx| {
                    panel.tool_setup.choosing = false;
                    match result {
                        Ok(true) => panel.error = None,
                        Ok(false) => {}
                        Err(error) => panel.fail(error, window, cx),
                    }
                    panel.refresh_tool_setup(cx);
                    panel.refresh_devices(cx);
                })
                .log_err();
        })
        .detach();
    }

    pub(super) fn manage_tool(
        &mut self,
        tool: Tool,
        operation: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.running
            || self.syncing
            || self.tool_setup.choosing
            || self.tool_setup.operation.is_some()
        {
            return;
        }
        let root = match self.trusted_root(cx) {
            Ok(root) => root,
            Err(error) => {
                self.fail(error, window, cx);
                return;
            }
        };
        if self.debug_forward.is_some() {
            self.fail(
                anyhow::anyhow!("Disconnect the Android debugger before changing managed tools."),
                window,
                cx,
            );
            return;
        }
        let offline = self.tool_setup.offline;
        self.root = Some(root.clone());
        self.last_build_operation = None;
        self.tool_setup.last_operation = Some((tool, operation));
        let label = format!("{}: {operation}", tool.label());
        let (session, output, logs) = self.build_panel.update(cx, |panel, cx| {
            panel.begin(BuildTab::Output, label.clone(), false, window, cx)
        });
        let (cancel, cancelled) = oneshot::channel();
        self.command_cancel = Some(cancel);
        self.active_build_session = Some((BuildTab::Output, session));
        self.running = true;
        self.error = None;
        self.status = label.into();
        let executor = cx.background_executor().clone();
        self.tool_setup.operation = Some(cx.spawn_in(window, async move |panel, cx| {
            let result = cx.background_spawn(async move {
                let prepared = managed::prepare(tool, operation, offline)?;
                let mut command = util::command::new_std_command(&prepared.program);
                command.args(&prepared.arguments).envs(&prepared.environment).current_dir(&root);
                let result = android_build::command_output_with_cleanup(command, &executor, Duration::from_secs(1800), output, cancelled, false).await;
                // Embedded recipes must remain available until every subprocess exits.
                drop(prepared.directory);
                result
            }).await;
            logs.await;
            panel.update_in(cx, |panel, window, cx| {
                if panel.active_build_session != Some((BuildTab::Output, session)) { return; }
                panel.active_build_session = None;
                panel.command_cancel = None;
                panel.running = false;
                panel.tool_setup.operation = None;
                let succeeded = matches!(&result, Ok(ProcessOutput::Success(_)));
                let (status, message) = match result {
                    Ok(ProcessOutput::Success(_)) => (BuildStatus::Succeeded, "Managed tool ready. Configure Kotlin or retry Run / Debug.".to_owned()),
                    Ok(ProcessOutput::Cancelled) => (BuildStatus::Cancelled, "Tool setup cancelled. The previous runtime is preserved; retry when ready.".to_owned()),
                    Err(error) => {
                        let message = format!("{error:#} See Build Output for the installer error. The previous runtime is preserved; repair and retry.");
                        panel.fail(anyhow::anyhow!(message.clone()), window, cx);
                        (BuildStatus::Failed, message)
                    }
                };
                panel.status = message.clone().into();
                panel.build_panel.update(cx, |panel, cx| panel.finish(BuildTab::Output, session, status, message, cx));
                if succeeded {
                    panel.managed_tool_succeeded(tool, operation, window, cx);
                } else {
                    panel.refresh_tool_setup(cx);
                }
                cx.notify();
            }).log_err();
        }));
        cx.notify();
    }

    fn render_tool_setup(&self, wizard: &SetupWizard, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.running || self.syncing || self.tool_setup.choosing;
        let details = v_flex().gap_2()
            .child(Label::new("Advanced Kotlin and debugger tools: Apple Silicon macOS. Preview uses the configured Java 21 runtime.").size(LabelSize::Small))
            .child(Label::new("These advanced installers require Python 3.12+ and Apple's Command Line Tools. Java and Android SDK setup uses Koda's native installer.").size(LabelSize::Small).color(Color::Muted))
            .child(Label::new("Verified pinned downloads: JetBrains Kotlin server, fwcd debugger sources and Adoptium JDK. Debugger builds also fetch Gradle dependencies over HTTPS.").size(LabelSize::Small).color(Color::Muted))
            .children(self.tool_setup.lines.iter().map(|line| Label::new(line.clone()).size(LabelSize::Small).line_clamp(4)))
            .child(h_flex().gap_1().flex_wrap()
                .child(wizard.focus_row("advanced-choose-sdk", setup_button("choose-sdk", "Choose SDK", cx).disabled(busy).tab_index(0isize).on_click(cx.listener(|panel, _, window, cx| panel.choose_dependency(Dependency::Sdk, window, cx))), cx))
                .child(wizard.focus_row("advanced-choose-jdk", setup_button("choose-jdk", "Choose JDK 21", cx).disabled(busy).tab_index(0isize).on_click(cx.listener(|panel, _, window, cx| panel.choose_dependency(Dependency::Jdk, window, cx))), cx))
                )
            .child(h_flex().gap_1().flex_wrap()
                .child(wizard.focus_row("advanced-check-tool-setup", setup_button("check-tool-setup", "Detect dependencies", cx).tab_index(0isize).on_click(cx.listener(|panel, _, _, cx| panel.refresh_tool_setup(cx))), cx))
                .child(wizard.focus_row("advanced-tool-storage", setup_button("tool-storage", "Reveal managed storage", cx).tab_index(0isize).on_click(|_, _, cx| cx.reveal_path(&managed::root())), cx))
                .child(wizard.focus_row("advanced-offline-tools", setup_button("offline-tools", if self.tool_setup.offline { "Offline: on" } else { "Offline: off" }, cx).tab_index(0isize).disabled(busy).on_click(cx.listener(|panel, _, _, cx| { panel.tool_setup.offline = !panel.tool_setup.offline; cx.notify(); })), cx)))
            .children(Tool::ALL.into_iter().map(|tool| {
                v_flex().gap_1().child(Label::new(tool.label())).child(wizard.focus_row(tool.name(), h_flex().gap_1().flex_wrap().children([
                    ("install", "Install / repair"), ("validate", "Validate"), ("rollback", "Roll back"),
                ].into_iter().map(|(operation, label)| {
                    setup_button(format!("{}-{operation}", tool.name()), label, cx).tab_index(0isize).disabled(busy || !managed::supported())
                        .on_click(cx.listener(move |panel, _, window, cx| panel.manage_tool(tool, operation, window, cx)))
                })), cx))
            }))
            .when(self.tool_setup.operation.is_some(), |element| element.child(wizard.focus_row("advanced-cancel-tool-setup",
                setup_button("cancel-tool-setup", "Cancel tool setup", cx).tab_index(0isize).on_click(cx.listener(|panel, _, _, cx| panel.cancel_build(BuildTab::Output, cx)))
            , cx)));
        v_flex()
            .gap_2()
            .child(
                wizard.focus_row(
                    "advanced-toggle-tool-setup",
                    setup_button(
                        "toggle-tool-setup",
                        if self.tool_setup.expanded {
                            "Advanced tools ▾"
                        } else {
                            "Advanced tools ▸"
                        },
                        cx,
                    )
                    .tab_index(0isize)
                    .on_click(cx.listener(|panel, _, _, cx| {
                        panel.tool_setup.expanded = !panel.tool_setup.expanded;
                        cx.notify();
                    })),
                    cx,
                ),
            )
            .when(self.tool_setup.expanded, |element| element.child(details))
    }
}
