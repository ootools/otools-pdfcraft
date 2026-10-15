//! PdfCraft 的 OTools 插件外壳。
//!
//! 本文件改编自上游 `apps/pdfcraft-web/src/main.rs`（上游 eframe web runner 的外壳）。
//! 改动只有三处，都是「把浏览器能力换成 OTools 宿主能力」：
//!
//! 1. `mod host`（嵌入页面的 postMessage 桥）→ `mod otools`（OTools 宿主桥，我们新写的）
//! 2. `app.host_save` 走 [`otools::deliver`]（写盘），否则退回上游的浏览器下载
//! 3. 首次启动时用 OTools 的界面主题给 egui 一个初始偏好
//!
//! 打开仍是上游的 rfd（浏览器里它用 `<input type=file>`），我们没法从外壳替换掉它：
//! 见 AGENTS.md 的「已知限制」。
//! 上游 `crates/*` 一律不改：它们按 upstream.pin 的 commit 拉取，保持原样才能干净地跟上上游。

#[cfg(target_arch = "wasm32")]
mod otools;

#[cfg(target_arch = "wasm32")]
mod web {
    use std::cell::{Cell, RefCell};

    use pdfcraft_ui_egui::{PdfCraftApp, theme::ThemePreference};
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::JsCast as _;

    const CANVAS_ID: &str = "pdfcraft";
    const LOADING_ID: &str = "pdfcraft_loading";

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
                        let mut app = PdfCraftApp::new();
                        // "printcraft"：以应用旧名保存过的设置。
                        if let Some(json) = cc.storage.and_then(|s| s.get_string("pdfcraft").or_else(|| s.get_string("printcraft"))) {
                            app.restore(&json);
                        }
                        if crate::otools::available() {
                            // 保存：交给宿主写盘（否则退回上游网页版的浏览器下载）。
                            let save_as_host: pdfcraft_ui_egui::HostSaveFn =
                                Box::new(|name: &str, bytes: &[u8], save_as: bool| crate::otools::deliver(name, bytes, save_as));
                            app.host_save = Some(save_as_host);
                            crate::otools::install(app.failed_inbox.clone(), cc.egui_ctx.clone());
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
