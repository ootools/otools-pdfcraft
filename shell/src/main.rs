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
fn main() {
    use pdfcraft_ui_egui::{PdfCraftApp, theme::ThemePreference};
    use wasm_bindgen::JsCast as _;

    const CANVAS_ID: &str = "pdfcraft";
    const LOADING_ID: &str = "pdfcraft_loading";

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
                    Ok(Box::new(app))
                }),
            )
            .await;
        if let Some(el) = document.get_element_by_id(LOADING_ID) {
            match result {
                Ok(()) => el.remove(),
                Err(e) => el.set_inner_html(&format!("<p>PdfCraft 启动失败：{e:?}</p><p>需要支持 WebGL2 的浏览器。</p>")),
            }
        }
    });
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    eprintln!("otools-pdfcraft 只在浏览器里运行：在 shell/ 目录用 `trunk build --release` 构建");
}
