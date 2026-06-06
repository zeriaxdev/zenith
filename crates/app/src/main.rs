use gpui::{
    div, hsla, linear_color_stop, linear_gradient, prelude::*, px, relative, size, App, Bounds,
    ClipboardItem, Context, FontWeight, Hsla, MouseButton, MouseDownEvent, Pixels, Point,
    ScrollHandle, SharedString, Window, WindowBounds, WindowOptions,
};
use gpui_platform::application;
use std::time::Duration;

use zenith_core::log as logbus;
use zenith_core::{Account, Loader, VersionEntry};
use zenith_launch as launch;
use zenith_net::{auth, mojang};
use zenith_store::Paths;
use zenith_ui::theme::*;

/// Best-effort: open the verification URL in the user's browser.
fn open_url(url: &str) {
    let _ = std::process::Command::new("open").arg(url).spawn();
}

// ---- Navigation ----------------------------------------------------------
#[derive(Clone, Copy, PartialEq)]
enum View {
    Home,
    Instances,
    Mods,
    Console,
    Settings,
}

impl View {
    fn label(self) -> &'static str {
        match self {
            View::Home => "Home",
            View::Instances => "Instances",
            View::Mods => "Mods",
            View::Console => "Console",
            View::Settings => "Settings",
        }
    }
}

// ---- Toasts --------------------------------------------------------------
#[derive(Clone, Copy, PartialEq)]
enum ToastKind {
    Info,
    Success,
    Error,
}

impl ToastKind {
    fn color(self) -> Hsla {
        match self {
            ToastKind::Info => hsla(210. / 360., 0.55, 0.58, 1.),
            ToastKind::Success => accent_hi(),
            ToastKind::Error => hsla(2. / 360., 0.65, 0.60, 1.),
        }
    }
}

struct Toast {
    id: u64,
    kind: ToastKind,
    title: SharedString,
    message: SharedString,
}

/// Sign-in state machine for the account card.
enum Auth {
    Out,
    Pending { code: SharedString, url: SharedString },
    In { name: SharedString, uuid: SharedString },
    Failed(SharedString),
}

#[derive(Clone, Copy, PartialEq)]
enum RunState {
    Idle,
    Starting,
    Running,
}

struct Launcher {
    entries: Vec<VersionEntry>,
    selected: usize,
    status: SharedString,
    run: RunState,
    auth: Auth,
    toasts: Vec<Toast>,
    next_toast_id: u64,
    view: View,
    console_scroll: ScrollHandle,
    console_follow: bool,
    console_menu: Option<Point<Pixels>>,
    loader: Loader,
}

impl Launcher {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            selected: 0,
            status: "Loading versions…".into(),
            run: RunState::Idle,
            auth: Auth::Out,
            toasts: Vec::new(),
            next_toast_id: 0,
            view: View::Home,
            console_scroll: ScrollHandle::new(),
            console_follow: true,
            console_menu: None,
            loader: Loader::Vanilla,
        }
    }

    /// Keep the console pinned to the newest line, unless the user has
    /// scrolled up — then leave their position alone until they return.
    fn console_autoscroll(&mut self) {
        let off = self.console_scroll.offset().y; // 0 at top, -max at bottom
        let max = self.console_scroll.max_offset().y;
        // distance from the bottom (0 when fully scrolled down)
        self.console_follow = (max + off) <= px(24.);
        if self.console_follow {
            self.console_scroll.scroll_to_bottom();
        }
    }

    /// Re-render periodically so the Console reflects async log output.
    fn start_tick(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(400))
                .await;
            let ok = this
                .update(cx, |this, cx| {
                    // Reconcile run state with the actual process.
                    if this.run == RunState::Running && !launch::is_running() {
                        this.run = RunState::Idle;
                        this.status = "Ready".into();
                    }
                    // Only spend a frame when something is actually moving:
                    // a download/launch in progress, a running game, or the
                    // live console being viewed.
                    let busy = this.run != RunState::Idle
                        || logbus::progress().is_some()
                        || this.view == View::Console;
                    if busy {
                        if this.view == View::Console {
                            this.console_autoscroll();
                        }
                        cx.notify();
                    }
                })
                .is_ok();
            if !ok {
                break;
            }
        })
        .detach();
    }

    /// Fetch the Mojang version manifest in the background (releases only).
    fn load_versions(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async { mojang::fetch_versions() })
                .await;
            let _ = this.update(cx, |this, cx| {
                match res {
                    Ok(list) => {
                        this.entries =
                            list.into_iter().filter(|v| v.kind == "release").collect();
                        this.status =
                            format!("{} versions available", this.entries.len()).into();
                        logbus::info(format!("Loaded {} releases.", this.entries.len()));
                    }
                    Err(e) => {
                        this.status = "Failed to load versions".into();
                        this.toast(
                            ToastKind::Error,
                            "Version list failed",
                            e.to_string(),
                            true,
                            cx,
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Stop the running game.
    fn stop(&mut self, cx: &mut Context<Self>) {
        if self.run != RunState::Running {
            return;
        }
        launch::kill_running();
        self.status = "Stopping…".into();
        self.toast(ToastKind::Info, "Stopping", "Closing the game…", false, cx);
        cx.notify();
    }

    /// Download (if needed) and launch the selected version.
    fn play(&mut self, cx: &mut Context<Self>) {
        if self.run != RunState::Idle {
            return;
        }
        let Some(entry) = self.entries.get(self.selected).cloned() else {
            self.toast(
                ToastKind::Error,
                "No version",
                "Versions haven't loaded yet.",
                false,
                cx,
            );
            return;
        };

        // Auth is parked behind Microsoft's API gate, so launch offline.
        let name = match &self.auth {
            Auth::In { name, .. } => name.to_string(),
            _ => "Player".to_string(),
        };
        let account = Account::offline(&name);
        let paths = Paths::new();
        let loader = self.loader;

        self.run = RunState::Starting;
        self.status = format!("Starting {} · {}…", entry.id, loader.label()).into();
        self.toast(
            ToastKind::Info,
            "Starting",
            format!("Preparing {} · {} (first run downloads files)", entry.id, loader.label()),
            false,
            cx,
        );
        logbus::info(format!(
            "Play requested: {} {} as {}",
            entry.id,
            loader.label(),
            name
        ));
        cx.notify();

        cx.spawn(async move |this, cx| {
            let entry_bg = entry.clone();
            let paths_bg = paths.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let prepared = launch::prepare(&entry_bg, loader, &paths_bg)?;
                    launch::launch(&prepared, &account, &paths_bg)?;
                    anyhow::Ok(())
                })
                .await;

            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.run = RunState::Running;
                        this.status = format!("Running {}", entry.id).into();
                        this.toast(
                            ToastKind::Success,
                            "Running",
                            format!("{} is running", entry.id),
                            false,
                            cx,
                        );
                    }
                    Err(e) => {
                        this.run = RunState::Idle;
                        this.status = "Launch failed".into();
                        this.toast(ToastKind::Error, "Launch failed", e.to_string(), true, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Add a toast. Non-sticky toasts auto-dismiss after a few seconds.
    fn toast(
        &mut self,
        kind: ToastKind,
        title: impl Into<SharedString>,
        message: impl Into<SharedString>,
        sticky: bool,
        cx: &mut Context<Self>,
    ) {
        let id = self.next_toast_id;
        self.next_toast_id += 1;
        self.toasts.push(Toast {
            id,
            kind,
            title: title.into(),
            message: message.into(),
        });
        cx.notify();

        if !sticky {
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_secs(4))
                    .await;
                let _ = this.update(cx, |this, cx| {
                    this.dismiss_toast(id);
                    cx.notify();
                });
            })
            .detach();
        }
    }

    fn dismiss_toast(&mut self, id: u64) {
        self.toasts.retain(|t| t.id != id);
    }

    /// Kick off the Microsoft device-code login on a background task,
    /// updating `self.auth` as each stage completes.
    fn sign_in(&mut self, cx: &mut Context<Self>) {
        self.auth = Auth::Pending {
            code: "…".into(),
            url: "requesting code…".into(),
        };
        cx.notify();

        let client_id = auth::client_id();
        cx.spawn(async move |this, cx| {
            // Step 1: device code (off the UI thread)
            let cid = client_id.clone();
            let dc = cx
                .background_executor()
                .spawn(async move { auth::request_device_code(&cid) })
                .await;
            let dc = match dc {
                Ok(dc) => dc,
                Err(e) => {
                    let _ = this.update(cx, |this, cx| {
                        let msg: SharedString = e.to_string().into();
                        this.auth = Auth::Failed(msg.clone());
                        this.toast(ToastKind::Error, "Sign-in failed", msg, true, cx);
                        cx.notify();
                    });
                    return;
                }
            };

            // Show the code + open the browser.
            open_url(&dc.verification_uri);
            let _ = this.update(cx, |this, cx| {
                this.auth = Auth::Pending {
                    code: dc.user_code.clone().into(),
                    url: dc.verification_uri.clone().into(),
                };
                this.toast(
                    ToastKind::Info,
                    "Your sign-in code",
                    dc.user_code.clone(),
                    true,
                    cx,
                );
                cx.notify();
            });

            // Step 2: poll for the Microsoft token.
            let cid = client_id.clone();
            let token = cx
                .background_executor()
                .spawn(async move { auth::poll_for_token(&cid, &dc) })
                .await;
            let token = match token {
                Ok(t) => t,
                Err(e) => {
                    let _ = this.update(cx, |this, cx| {
                        let msg: SharedString = e.to_string().into();
                        this.auth = Auth::Failed(msg.clone());
                        this.toast(ToastKind::Error, "Sign-in failed", msg, true, cx);
                        cx.notify();
                    });
                    return;
                }
            };

            // Step 3: exchange for a Minecraft session.
            let session = cx
                .background_executor()
                .spawn(async move { auth::minecraft_login(&token) })
                .await;
            let _ = this.update(cx, |this, cx| {
                match session {
                    Ok(s) => {
                        let name: SharedString = s.username.into();
                        this.toast(
                            ToastKind::Success,
                            "Signed in",
                            format!("Welcome, {name}"),
                            false,
                            cx,
                        );
                        this.auth = Auth::In {
                            name,
                            uuid: s.uuid.into(),
                        };
                    }
                    Err(e) => {
                        let msg: SharedString = e.to_string().into();
                        this.auth = Auth::Failed(msg.clone());
                        this.toast(ToastKind::Error, "Sign-in failed", msg, true, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn cycle(&mut self, delta: isize) {
        let n = self.entries.len() as isize;
        if n == 0 {
            return;
        }
        self.selected = (((self.selected as isize + delta) % n + n) % n) as usize;
    }

    fn current(&self) -> SharedString {
        self.entries
            .get(self.selected)
            .map(|e| SharedString::from(e.id.clone()))
            .unwrap_or_else(|| "—".into())
    }

    fn step_btn(
        &self,
        id: &'static str,
        glyph: &'static str,
        delta: isize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(id)
            .size(px(38.))
            .flex()
            .items_center()
            .justify_center()
            .rounded_lg()
            .bg(card())
            .border_1()
            .border_color(border())
            .text_color(muted())
            .hover(|s| s.bg(card_hi()).text_color(text()))
            .active(|s| s.bg(border()))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.cycle(delta);
                cx.notify();
            }))
            .child(glyph)
    }

    /// A small clickable chip that copies `payload` to the clipboard.
    fn copy_chip(
        &self,
        key: &'static str,
        label: impl Into<SharedString>,
        payload: String,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(key)
            .cursor_pointer()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(card_hi())
            .text_xs()
            .text_color(text())
            .hover(|s| s.bg(border()))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(payload.clone()));
                this.toast(ToastKind::Success, "Copied", "Copied to clipboard", false, cx);
            }))
            .child(label.into())
    }

    fn account_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // avatar tile + two lines of text describing current auth state
        let (avatar, line1, line2): (Hsla, SharedString, SharedString) = match &self.auth {
            Auth::Out => (card_hi(), "Not signed in".into(), "Offline".into()),
            Auth::Pending { code, .. } => (
                hsla(40. / 360., 0.6, 0.5, 1.),
                "Signing in…".into(),
                format!("Code: {code}").into(),
            ),
            Auth::In { name, .. } => (
                hsla(28. / 360., 0.55, 0.45, 1.),
                name.clone(),
                "Microsoft account".into(),
            ),
            Auth::Failed(_) => (
                hsla(0. / 360., 0.55, 0.5, 1.),
                "Sign-in failed".into(),
                "Tap to retry".into(),
            ),
        };

        let mut card_el = col()
            .gap_3()
            .p_3()
            .rounded_xl()
            .bg(card())
            .border_1()
            .border_color(border())
            .child(
                row()
                    .gap_3()
                    .child(div().size(px(34.)).rounded_lg().bg(avatar))
                    .child(
                        col()
                            .child(div().text_sm().child(line1))
                            .child(div().text_xs().text_color(muted()).child(line2)),
                    ),
            );

        // While pending, surface copyable chips for the code and the link.
        if let Auth::Pending { url, code } = &self.auth {
            card_el = card_el
                .child(self.copy_chip(
                    "copy-code",
                    format!("⧉  Copy code: {code}"),
                    code.to_string(),
                    cx,
                ))
                .child(self.copy_chip("copy-url", "⧉  Copy sign-in link", url.to_string(), cx));
        }
        if let Auth::Failed(msg) = &self.auth {
            card_el = card_el.child(
                div()
                    .text_xs()
                    .text_color(hsla(0., 0.5, 0.65, 1.))
                    .child(msg.clone()),
            );
        }

        let pending = matches!(self.auth, Auth::Pending { .. });
        let signed_in = matches!(self.auth, Auth::In { .. });

        card_el.child(
            div()
                .id("signin")
                .h(px(34.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_lg()
                .text_sm()
                .bg(card_hi())
                .text_color(text())
                .when(!pending, |s| s.hover(|s| s.bg(border())))
                .when(pending, |s| s.text_color(muted()))
                .on_click(cx.listener(|this, _, _, cx| {
                    if matches!(this.auth, Auth::Pending { .. }) {
                        return; // already in progress
                    }
                    if matches!(this.auth, Auth::In { .. }) {
                        this.auth = Auth::Out;
                        cx.notify();
                    } else {
                        this.sign_in(cx);
                    }
                }))
                .child(if signed_in {
                    "Sign out"
                } else if pending {
                    "Waiting…"
                } else {
                    "Sign in with Microsoft"
                }),
        )
    }

    // bottom-right stack of toasts, overlaid on top of everything
    fn render_toasts(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .absolute()
            .bottom(px(16.))
            .right(px(16.))
            .flex()
            .flex_col_reverse()
            .gap_2()
            .children(self.toasts.iter().map(|t| self.render_toast(t, cx)))
    }

    fn render_toast(&self, t: &Toast, cx: &mut Context<Self>) -> impl IntoElement {
        let id = t.id;
        let accent_col = t.kind.color();
        let message = t.message.clone();
        let copy_payload = t.message.to_string();
        let copy_payload_body = t.message.to_string();

        // small square icon-button used for copy / close
        let mini = |key: SharedString, glyph: &'static str| {
            div()
                .id(key)
                .size(px(22.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_md()
                .text_xs()
                .text_color(muted())
                .hover(|s| s.bg(card_hi()).text_color(text()))
                .child(glyph)
        };

        div()
            .id(SharedString::from(format!("toast-{id}")))
            .w(px(340.))
            .flex()
            .flex_row()
            .overflow_hidden()
            .rounded_lg()
            .bg(card())
            .border_1()
            .border_color(border())
            .shadow_lg()
            // left accent strip (stretches to full height)
            .child(div().w(px(4.)).bg(accent_col))
            .child(
                col()
                    .flex_1()
                    .p_3()
                    .gap_1()
                    .child(
                        row()
                            .justify_between()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(accent_col)
                                    .child(t.title.clone()),
                            )
                            .child(
                                row()
                                    .gap_1()
                                    .child(
                                        mini(format!("toast-copy-{id}").into(), "Copy").on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                cx.write_to_clipboard(ClipboardItem::new_string(
                                                    copy_payload.clone(),
                                                ));
                                                this.toast(
                                                    ToastKind::Success,
                                                    "Copied",
                                                    "Copied to clipboard",
                                                    false,
                                                    cx,
                                                );
                                            }),
                                        ),
                                    )
                                    .child(mini(format!("toast-close-{id}").into(), "✕").on_click(
                                        cx.listener(move |this, _, _, cx| {
                                            this.dismiss_toast(id);
                                            cx.notify();
                                        }),
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("toast-body-{id}")))
                            .cursor_pointer()
                            .text_sm()
                            .text_color(text())
                            .hover(|s| s.text_color(accent_col))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(
                                    copy_payload_body.clone(),
                                ));
                                this.toast(
                                    ToastKind::Success,
                                    "Copied",
                                    "Copied to clipboard",
                                    false,
                                    cx,
                                );
                            }))
                            .child(message),
                    ),
            )
    }

    fn nav_item(&self, target: View, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.view == target;
        let dot = div()
            .size(px(6.))
            .rounded_full()
            .bg(if active { accent() } else { hsla(0., 0., 0., 0.) });
        let base = row()
            .id(target.label())
            .gap_3()
            .px_3()
            .py_2()
            .rounded_lg()
            .text_sm()
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.view = target;
                cx.notify();
            }));
        if active {
            base.bg(card())
                .text_color(text())
                .child(dot)
                .child(target.label())
        } else {
            base.text_color(muted())
                .hover(|s| s.bg(panel()).text_color(text()))
                .child(dot)
                .child(target.label())
        }
    }

    // top bar + the active view's body
    fn render_main(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.view {
            View::Home => self.render_home(cx).into_any_element(),
            View::Console => self.render_console(cx).into_any_element(),
            other => self.render_placeholder(other).into_any_element(),
        };

        col()
            .flex_1()
            .h_full()
            .min_h_0()
            .min_w_0()
            .child(
                row()
                    .flex_none()
                    .h(px(56.))
                    .px_6()
                    .border_b_1()
                    .border_color(border())
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(self.view.label()),
                    ),
            )
            .children(self.render_progress())
            .child(body)
    }

    /// A thin global progress bar, shown only while a download is running.
    fn render_progress(&self) -> Option<impl IntoElement> {
        let (done, total, label) = logbus::progress()?;
        let frac = (done as f32 / total as f32).clamp(0., 1.);
        let pct = (frac * 100.) as u32;
        Some(
            col()
                .px_6()
                .py_2()
                .gap_1()
                .bg(panel())
                .border_b_1()
                .border_color(border())
                .child(
                    row()
                        .justify_between()
                        .text_xs()
                        .text_color(muted())
                        .child(format!("{label} · {pct}%"))
                        .child(format!("{done}/{total}")),
                )
                .child(
                    div()
                        .h(px(6.))
                        .w_full()
                        .rounded_full()
                        .bg(card())
                        .child(
                            div()
                                .h_full()
                                .w(relative(frac))
                                .rounded_full()
                                .bg(accent()),
                        ),
                ),
        )
    }

    fn render_placeholder(&self, view: View) -> impl IntoElement {
        col()
            .flex_1()
            .items_center()
            .justify_center()
            .gap_2()
            .child(
                div()
                    .text_color(text())
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(view.label()),
            )
            .child(div().text_color(muted()).child("Coming soon."))
    }

    fn loader_chip(&self, loader: Loader, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.loader == loader;
        div()
            .id(loader.label())
            .px_3()
            .py_1p5()
            .rounded_lg()
            .text_sm()
            .border_1()
            .cursor_pointer()
            .when(active, |s| {
                s.bg(card_hi()).border_color(accent()).text_color(text())
            })
            .when(!active, |s| {
                s.bg(card())
                    .border_color(border())
                    .text_color(muted())
                    .hover(|s| s.bg(card_hi()).text_color(text()))
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.loader = loader;
                cx.notify();
            }))
            .child(loader.label())
    }

    fn render_home(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let version = self.current();
        let status = self.status.clone();
        col()
            .flex_1()
            .p_5()
            .gap_5()
            // hero
            .child(
                col()
                    .flex_1()
                    .justify_end()
                    .p_8()
                    .rounded_2xl()
                    .border_1()
                    .border_color(border())
                    .bg(linear_gradient(
                        165.,
                        linear_color_stop(card(), 0.),
                        linear_color_stop(panel(), 1.),
                    ))
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted())
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("SELECTED VERSION"),
                    )
                    .child(
                        div()
                            .mt_1()
                            .text_size(px(40.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(text())
                            .child("Minecraft"),
                    )
                    .child(
                        div()
                            .text_color(muted())
                            .child(format!("Java Edition · {} · {}", version, self.loader.label())),
                    ),
            )
            // loader selector
            .child(
                row().flex_none().gap_2().children(
                    Loader::all()
                        .into_iter()
                        .map(|l| self.loader_chip(l, cx)),
                ),
            )
            // bottom play bar
            .child(
                row()
                    .justify_between()
                    .p_3()
                    .rounded_xl()
                    .bg(panel())
                    .border_1()
                    .border_color(border())
                    .child(
                        row()
                            .gap_2()
                            .child(self.step_btn("prev", "<", -1, cx))
                            .child(
                                col()
                                    .w(px(150.))
                                    .items_center()
                                    .child(div().text_xs().text_color(muted()).child("VERSION"))
                                    .child(
                                        div()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(self.current()),
                                    ),
                            )
                            .child(self.step_btn("next", ">", 1, cx)),
                    )
                    .child(
                        col()
                            .items_end()
                            .gap_1()
                            .child(self.render_play_button(cx))
                            .child(div().text_xs().text_color(muted()).child(status)),
                    ),
            )
    }

    /// Foolproof primary action: Play (idle) / Starting… (disabled) / Stop (running).
    fn render_play_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let base = div()
            .id("play")
            .w(px(220.))
            .h(px(52.))
            .flex()
            .items_center()
            .justify_center()
            .rounded_xl()
            .font_weight(FontWeight::SEMIBOLD)
            .text_size(px(16.));

        match self.run {
            RunState::Idle => base
                .bg(accent())
                .text_color(white())
                .cursor_pointer()
                .hover(|s| s.bg(accent_hi()))
                .on_click(cx.listener(|this, _, _, cx| this.play(cx)))
                .child("Play"),
            RunState::Starting => base
                .bg(card_hi())
                .text_color(muted())
                .cursor_default()
                .child("Starting…"),
            RunState::Running => base
                .bg(danger())
                .text_color(white())
                .cursor_pointer()
                .hover(|s| s.bg(danger_hi()))
                .on_click(cx.listener(|this, _, _, cx| this.stop(cx)))
                .child("Stop"),
        }
    }

    fn render_console(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // Only render the most recent lines — building an interactive element
        // per line for thousands of game-output lines every tick is what
        // previously hung/crashed the app.
        const VISIBLE: usize = 400;
        let count = logbus::len();
        let lines = logbus::tail(VISIBLE);

        let btn = |key: &'static str, label: &'static str| {
            div()
                .id(key)
                .px_3()
                .py_1()
                .rounded_md()
                .bg(card())
                .border_1()
                .border_color(border())
                .text_xs()
                .text_color(text())
                .cursor_pointer()
                .hover(|s| s.bg(card_hi()))
                .child(label)
        };

        col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .p_5()
            .gap_3()
            // toolbar
            .child(
                row()
                    .flex_none()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted())
                            .child(if count > VISIBLE {
                                format!("{count} lines (showing last {VISIBLE})")
                            } else {
                                format!("{count} lines")
                            }),
                    )
                    .child(
                        row()
                            .gap_2()
                            .child(btn("console-copy", "Copy all").on_click(cx.listener(
                                |this, _, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        logbus::all_text(),
                                    ));
                                    this.toast(
                                        ToastKind::Success,
                                        "Copied",
                                        "Console copied to clipboard",
                                        false,
                                        cx,
                                    );
                                },
                            )))
                            .child(btn("console-clear", "Clear").on_click(cx.listener(
                                |_, _, _, cx| {
                                    logbus::clear();
                                    cx.notify();
                                },
                            ))),
                    ),
            )
            // log output
            .child(
                div()
                    .id("console-scroll")
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .w_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.console_scroll)
                    .p_3()
                    .rounded_lg()
                    .bg(hsla(222. / 360., 0.16, 0.07, 1.))
                    .border_1()
                    .border_color(border())
                    .font_family("IBM Plex Mono")
                    .text_xs()
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|this, ev: &MouseDownEvent, _, cx| {
                            this.console_menu = Some(ev.position);
                            cx.notify();
                        }),
                    )
                    .children(lines.into_iter().enumerate().map(|(i, line)| {
                        let color = match line.level {
                            logbus::Level::Error => hsla(2. / 360., 0.7, 0.66, 1.),
                            logbus::Level::Warn => hsla(40. / 360., 0.8, 0.62, 1.),
                            logbus::Level::Info => muted(),
                            logbus::Level::Game => text(),
                        };
                        let payload = line.text.clone();
                        div()
                            .id(SharedString::from(format!("console-line-{i}")))
                            .py_0p5()
                            .w_full()
                            .whitespace_normal()
                            .cursor_pointer()
                            .text_color(color)
                            .hover(|s| s.bg(card()))
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(payload.clone()));
                                cx.notify();
                            }))
                            .child(line.text)
                    })),
            )
    }

    /// Right-click menu for the console (anchored at the click position).
    fn render_context_menu(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let pos = self.console_menu?;
        let item = |key: &'static str, label: &'static str| {
            div()
                .id(key)
                .px_3()
                .py_1p5()
                .rounded_md()
                .text_sm()
                .text_color(text())
                .cursor_pointer()
                .hover(|s| s.bg(card_hi()))
                .child(label)
        };
        Some(
            div()
                .absolute()
                .left(pos.x)
                .top(pos.y)
                .w(px(180.))
                .p_1()
                .rounded_lg()
                .bg(card())
                .border_1()
                .border_color(border())
                .shadow_lg()
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.console_menu = None;
                    cx.notify();
                }))
                .child(item("ctx-copy", "Copy all").on_click(cx.listener(|this, _, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(logbus::all_text()));
                    this.console_menu = None;
                    this.toast(ToastKind::Success, "Copied", "Console copied", false, cx);
                })))
                .child(item("ctx-bottom", "Scroll to bottom").on_click(cx.listener(
                    |this, _, _, cx| {
                        this.console_follow = true;
                        this.console_scroll.scroll_to_bottom();
                        this.console_menu = None;
                        cx.notify();
                    },
                )))
                .child(item("ctx-clear", "Clear").on_click(cx.listener(|this, _, _, cx| {
                    logbus::clear();
                    this.console_menu = None;
                    cx.notify();
                }))),
        )
    }
}

impl Render for Launcher {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        row()
            .relative()
            .size_full()
            .bg(bg())
            .text_color(text())
            .font_family("IBM Plex Sans")
            .text_sm()
            // ============ Sidebar ============
            .child(
                col()
                    .w(px(232.))
                    .h_full()
                    .bg(panel())
                    .border_r_1()
                    .border_color(border())
                    .p_3()
                    .gap_1()
                    // brand
                    .child(
                        row()
                            .gap_3()
                            .px_2()
                            .py_3()
                            .mb_3()
                            .child(
                                div()
                                    .size(px(34.))
                                    .rounded_lg()
                                    .bg(linear_gradient(
                                        145.,
                                        linear_color_stop(accent_hi(), 0.),
                                        linear_color_stop(accent(), 1.),
                                    ))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_color(white())
                                    .font_weight(FontWeight::BOLD)
                                    .child("Z"),
                            )
                            .child(
                                col()
                                    .child(
                                        div()
                                            .text_color(text())
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child("Zenith"),
                                    )
                                    .child(div().text_xs().text_color(muted()).child("Launcher")),
                            ),
                    )
                    .child(self.nav_item(View::Home, cx))
                    .child(self.nav_item(View::Instances, cx))
                    .child(self.nav_item(View::Mods, cx))
                    .child(self.nav_item(View::Console, cx))
                    .child(self.nav_item(View::Settings, cx))
                    .child(div().flex_1())
                    .child(self.account_card(cx)),
            )
            // ============ Main ============
            .child(self.render_main(cx))
            // ============ Toast overlay ============
            .child(self.render_toasts(cx))
            // ============ Right-click menu ============
            .children(self.render_context_menu(cx))
    }
}

use gpui::{actions, KeyBinding, Menu, MenuItem};

actions!(zenith, [Quit]);

fn quit(_: &Quit, cx: &mut App) {
    cx.quit();
}

fn main() {
    sheen::init_with(
        sheen::Logger::new()
            .level(sheen::Level::Info)
            .colorize(true)
            .timestamp(true)
            .prefix("zenith"),
    );

    application().run(|cx: &mut App| {
        // Quit support: app menu + ⌘Q, and quit when the last window closes.
        cx.on_action(quit);
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
        cx.set_menus(vec![
            Menu::new("Zenith").items([MenuItem::action("Quit Zenith", Quit)]),
        ]);
        cx.on_window_closed(|cx, _window_id| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        // Bundle IBM Plex Mono (what Zed's "Zed Plex Mono" is based on) so the
        // Console matches Zed's look without depending on a system install.
        let _ = cx.text_system().add_fonts(vec![
            std::borrow::Cow::Borrowed(
                include_bytes!("../../../assets/fonts/IBMPlexSans-Regular.ttf").as_slice(),
            ),
            std::borrow::Cow::Borrowed(
                include_bytes!("../../../assets/fonts/IBMPlexSans-Medium.ttf").as_slice(),
            ),
            std::borrow::Cow::Borrowed(
                include_bytes!("../../../assets/fonts/IBMPlexSans-SemiBold.ttf").as_slice(),
            ),
            std::borrow::Cow::Borrowed(
                include_bytes!("../../../assets/fonts/IBMPlexMono-Regular.ttf").as_slice(),
            ),
            std::borrow::Cow::Borrowed(
                include_bytes!("../../../assets/fonts/IBMPlexMono-Medium.ttf").as_slice(),
            ),
        ]);

        let bounds = Bounds::centered(None, size(px(940.), px(620.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| {
                cx.new(|cx| {
                    let mut launcher = Launcher::new();
                    launcher.start_tick(cx);
                    launcher.load_versions(cx);
                    launcher
                })
            },
        )
        .unwrap();
        cx.activate(true);
    });
}
