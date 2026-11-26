//! PdfCraft 的 OTools 外壳：安装 craft UI runtime 与文件服务，恢复偏好，
//! 跟随宿主主题和语言，将宿主投递文件送进应用、保存结果送回宿主。
//! 上游保持 pin 快照，定制字体与社区入口通过 shell/patches 重放。

#[cfg(any(target_arch = "wasm32", test))]
mod requests;

#[cfg(target_arch = "wasm32")]
mod otools;

#[cfg(target_arch = "wasm32")]
mod web {
    use std::cell::{Cell, RefCell};

    use pdfcraft_ui_egui::{PdfCraftApp, AppServices};
    use craft::theme::ModeChoice as ThemePreference;
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::JsCast as _;

    const CANVAS_ID: &str = "pdfcraft";
    const LOADING_ID: &str = "pdfcraft_loading";

    /// OTools 的默认布局：导航面板（书签、缩略图、注释……）靠左，工具面板靠右。
    ///
    /// 上游默认是「工具在左、导航在右」，与国内用户习惯相反。这里用上游暴露的公开 API
    /// （`side_panels::state` / `command`；均经 `pub use panels::side_panels` 导出）把两者对调：
    /// 把导航轨道移到左（`rail_side=left`），把工具面板移到右（`panel=tools, edge=right`）。
    ///
    /// 只在当前仍是「工具左 + 导航右」这一上游默认朝向时才翻转。用户一旦在「选项」里
    /// 自己改过（工具已在右，或导航已在左），就不再干预——既尊重用户选择，也保证幂等。
    fn apply_otools_default_layout(app: &mut PdfCraftApp) {
        use pdfcraft_ui_egui::side_panels;

        let st = side_panels::state(app);
        let tools_left = st.get("tools_side").and_then(|v| v.as_str()) == Some("left");
        let rail_right = st.get("rail_side").and_then(|v| v.as_str()) == Some("right");
        if !(tools_left && rail_right) {
            return;
        }
        // 先挪工具到右边，再把导航轨道挪到左边；两者都经上游的 command 走同一套校验。
        let _ = side_panels::command(app, false, &serde_json::json!({ "panel": "tools", "edge": "right" }));
        let _ = side_panels::command(app, false, &serde_json::json!({ "rail_side": "left" }));
    }

    /// 包一层应用：注入「跟随宿主语言」与「屏蔽社区外链」，其余原样转发给 `PdfCraftApp`。
    struct WebShell {
        app: PdfCraftApp,
    }

    thread_local! {
        /// 宿主语言切换后，下一帧把 host locale 写进 interface_language 偏好并重渲染。
        static NEEDS_LANG_SYNC: Cell<bool> = const { Cell::new(true) };
        /// 缓存 egui Context，供语言切换事件触发重绘。
        static APP_CTX: RefCell<Option<egui::Context>> = const { RefCell::new(None) };
    }

    impl eframe::App for WebShell {
        fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
            self.app.raw_input_hook(ctx, raw);
        }

        fn save(&mut self, storage: &mut dyn eframe::Storage) {
            self.app.save(storage);
        }

        fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
            APP_CTX.with(|c| *c.borrow_mut() = Some(ctx.clone()));
            // 跟随宿主语言：切换后下一帧把 host locale 写进 interface_language 偏好（重渲染）。
            if NEEDS_LANG_SYNC.with(|f| f.replace(false)) {
                let code = host_lang_code(&crate::otools::host_language());
                if self.app.language != code {
                    self.app.language = code;
                }
            }
            self.app.logic(ctx, frame);
            // 宿主投递的「打开方式」：系统里双击已绑定扩展名的 PDF → 用本插件打开。
            // 字节由 JS 侧读好（`otools::drain_delivered`），这里只负责交给上游。
            for file in crate::otools::drain_delivered() {
                if let Err(error) = self.app.open_bytes(ctx, &file.name, file.path, file.bytes) {
                    log::warn!("could not open delivered {}: {error}", file.name);
                }
            }
            // 让插件更纯粹：吞掉所有社区/外部链接（Discord、ArtCraft 官网、上游 GitHub），
            // 点击不再跳出插件。文档内普通链接不受影响。
            ctx.output_mut(|o| {
                o.commands.retain(|c| match c {
                    egui::OutputCommand::OpenUrl(u) => !crate::otools::is_community_url(&u.url),
                    _ => true,
                });
            });
        }

        fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
            self.app.ui(ui, frame);
        }
    }

    pub fn start() {
        eframe::WebLogger::init(log::LevelFilter::Info).ok();
        // 上游网页版就是这个渲染后端（Glow/WebGL2），别换成 wgpu。
        let options = eframe::WebOptions { renderer: eframe::Renderer::Glow, ..Default::default() };

        wasm_bindgen_futures::spawn_local(async move {
            let Some(document) = web_sys::window().and_then(|w| w.document()) else { return };
            let Some(canvas) = document.get_element_by_id(CANVAS_ID).and_then(|e| e.dyn_into::<web_sys::HtmlCanvasElement>().ok()) else {
                log::error!("missing <canvas id=\"{CANVAS_ID}\">");
                return;
            };
            let result = eframe::WebRunner::new()
                .start(
                    canvas,
                    options,
                    Box::new(|cc| {
                        pdfcraft_ui_egui::fonts::set_extra_fallback_fonts(vec![
                            craft::ui::fonts::EmbeddedFace { family: "OTools CJK", style: "Regular", scripts: &["Hans", "Hant", "Jpan", "Kore"], index: 0, bytes: include_bytes!("../assets/fonts/otools-cjk-subset.ttf"), role: None },
                            craft::ui::fonts::EmbeddedFace { family: "OTools Arabic", style: "Regular", scripts: &["Arab"], index: 0, bytes: include_bytes!("../assets/fonts/otools-arabic-subset.ttf"), role: None },
                        ]);
                        let settings = pdfcraft_ui_egui::settings::open();
                        let lang = pdfcraft_ui_egui::i18n::Lang::from_pref(&host_lang_code(&crate::otools::host_language()));
                        let ctx = cc.egui_ctx.clone();
                        let system = craft::ui::runtime::env::SystemReader::start(move || ctx.request_repaint());
                        let spec = craft::ui::InstallSpec::new(
                            craft::host::target::ShellInfo::default(),
                            std::sync::Arc::new(craft::window::system::SystemWindows),
                        ).system(system);
                        craft::ui::shell::embed::install(&cc.egui_ctx, spec, &pdfcraft_ui_egui::fonts::ui_fonts(lang))
                            .map_err(|e| format!("UI runtime: {e}"))?;
                        let ctx = cc.egui_ctx.clone();
                        let wake = craft::host::Wake::new(move || ctx.request_repaint());
                        let (intake, _) = craft::platform::open::FileIntake::new(wake);
                        let files = crate::otools::HostFiles::new(cc.egui_ctx.clone());
                        let host = craft::host::Host::builder(pdfcraft_ui_egui::APP_INFO)
                            .files(std::sync::Arc::new(files)).build();
                        let mut app = PdfCraftApp::with_host(std::sync::Arc::new(host), AppServices::system(&cc.egui_ctx, intake));
                        app.apply_settings(&settings.get());
                        // OTools 默认布局：书签/缩略图等导航面板在左，工具面板在右（更符合用户习惯）。
                        // 上游默认是「工具左、导航右」；仅当用户尚未手动调整过（仍是上游默认朝向）时才翻转，
                        // 这样尊重用户在「选项」里自己选过的结果，也不会每次启动反复横跳。
                        apply_otools_default_layout(&mut app);
                        app.attach_settings(settings);
                        crate::otools::install(cc.egui_ctx.clone());
                        if crate::otools::available() {
                            // 用户还没在「选项」里定过主题时才跟随 OTools（插件 webview 的
                            // prefers-color-scheme 不一定等于宿主主题）。
                            if app.theme_preference == ThemePreference::System {
                                match crate::otools::theme().as_str() {
                                    "dark" => app.set_theme_preference(ThemePreference::Dark),
                                    "light" => app.set_theme_preference(ThemePreference::Light),
                                    _ => {}
                                }
                            }
                        }
                        Ok(Box::new(WebShell { app }))
                    }),
                )
                .await;
            // 监听宿主语言切换（OTools 注入的 `otools-locale-changed` 事件）。
            install_locale_listener();
            // 宿主投递了文件：把 egui 从空闲里叫醒，下一帧 `logic` 取件。
            install_deliver_listener();
            if let Some(el) = document.get_element_by_id(LOADING_ID) {
                match result {
                    Ok(()) => el.remove(),
                    Err(e) => el.set_inner_html(&format!("<p>PdfCraft 启动失败：{e:?}</p><p>需要支持 WebGL2 的浏览器。</p>")),
                }
            }
        });
    }

    /// 监听宿主语言切换（OTools 注入的 `otools-locale-changed` 事件），置位标志并请求重绘；
    /// 真正把 host locale 写进 interface_language 偏好发生在下一帧 `WebShell::logic`。
    fn install_locale_listener() {
        let Some(window) = web_sys::window() else { return };
        let closure = Closure::wrap(Box::new(|| {
            NEEDS_LANG_SYNC.with(|f| f.set(true));
            if let Some(ctx) = APP_CTX.with(|c| c.borrow().clone()) {
                ctx.request_repaint();
            }
        }) as Box<dyn FnMut()>);
        let _ = window.add_event_listener_with_callback(
            "otools-locale-changed",
            closure.as_ref().unchecked_ref::<js_sys::Function>(),
        );
        closure.forget();
    }

    /// 监听宿主投递（`shell/index.html` 读完字节后派发的 `otools-open-files-arrived`）：
    /// egui 空闲时不会跑帧，这里把上下文叫醒，让 `WebShell::logic` 去取件。
    fn install_deliver_listener() {
        let Some(window) = web_sys::window() else { return };
        let closure = Closure::wrap(Box::new(|| {
            if let Some(ctx) = APP_CTX.with(|c| c.borrow().clone()) {
                ctx.request_repaint();
            }
        }) as Box<dyn FnMut()>);
        let _ = window.add_event_listener_with_callback(
            "otools-open-files-arrived",
            closure.as_ref().unchecked_ref::<js_sys::Function>(),
        );
        closure.forget();
    }

    /// 把宿主的 BCP 47 locale 映射到 PdfCraft 的界面语言代码（共享 i18n 的 `Lang`）。
    /// `lang_from_tag` 会把 `zh-CN` 正确映射成 `zh-hans`，不支持的 locale 回退英文。
    fn host_lang_code(host: &str) -> String {
        pdfcraft_ui_egui::i18n::lang_from_tag(host)
            .map(|l| l.code().to_string())
            .unwrap_or_else(|| "en".to_string())
    }
}

#[cfg(target_arch = "wasm32")]
fn main() {
    web::start();
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    eprintln!("otools-pdfcraft 只在浏览器里运行：在 shell/ 目录用 `trunk build --release` 构建");
}
