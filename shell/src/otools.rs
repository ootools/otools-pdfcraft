//! OTools 宿主桥：把「文件存到哪去」换成 OTools 宿主的另存对话框与磁盘写入。
//!
//! 上游 `apps/pdfcraft-web` 在浏览器里没有文件系统：保存走浏览器下载。OTools 宿主注入的
//! `window.otools` 提供了真正的另存对话框与 `writeHostFile`（底层是宿主的
//! `tools_webview_pick_save_path` / `tools_webview_write_file`，无路径白名单限制），
//! 所以本模块是插件**唯一**需要新写的 Rust 代码：其余全部复用上游 crate。
//!
//! 与上游的对接点只有一个，就在上游设计好的钩子上，上游源码零改动：
//! - `PdfCraftApp::host_save` —— 保存时把字节交给宿主写盘
//!
//! 打开仍然是上游的 rfd（浏览器里它用 `<input type=file>`），因为我们没有 UI 层的
//! 钩子可以把「打开」菜单换掉；见 AGENTS.md 的「已知限制」。
//!
//! JS 侧的实现见 `shell/index.html` 里的 `window.pdfcraftOtools`。

use std::cell::RefCell;

use base64::Engine as _;
use pdfcraft_ui_egui::FailedInbox;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::js_sys;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = pdfcraftOtools, js_name = "available")]
    fn host_available() -> bool;

    #[wasm_bindgen(js_namespace = pdfcraftOtools, js_name = "theme")]
    fn host_theme() -> String;

    /// 宿主界面语言（BCP 47，如 `zh-CN`）：OTools 注入到插件 webview 的
    /// `window.__OTOOLS_LOCALE__.locale` / `document.documentElement.lang`。
    #[wasm_bindgen(js_namespace = pdfcraftOtools, js_name = "language")]
    fn host_language_raw() -> String;

    /// `{name, dataBase64, forcePick}` → `{path}` 或 `null`（用户取消）。
    #[wasm_bindgen(js_namespace = pdfcraftOtools, js_name = "saveFile")]
    fn host_save_file(request: JsValue) -> js_sys::Promise;
}

thread_local! {
    /// 保存失败时把错误送到应用的 failed_inbox（上游会显示在界面上）。
    static OUTBOX: RefCell<Option<(FailedInbox, egui::Context)>> = const { RefCell::new(None) };
}

/// 宿主是否可用（不在 OTools 里时退回上游的浏览器行为：下载文件）。
pub fn available() -> bool {
    host_available()
}

/// OTools 的界面主题，用于给 egui 一个初始主题（`""` = 跟随系统）。
pub fn theme() -> String {
    host_theme()
}

/// 宿主界面语言（BCP 47，如 `zh-CN`）：跟随 OTools 宿主语言切换。
pub fn host_language() -> String {
    host_language_raw()
}

/// 社区/外部链接（Discord、ArtCraft 官网、上游 GitHub）：在 OTools 插件里点这些
/// 会把用户拽到外部，让插件不纯粹。语言切换监听与链接屏蔽用到它。
pub fn is_community_url(url: &str) -> bool {
    url.contains("discord.gg") || url.contains("getartcraft.com") || url.contains("github.com/storytold")
}

/// 记住 failed_inbox 与 egui 上下文：异步保存的失败要在下一帧报给应用。
pub fn install(failed: FailedInbox, ctx: egui::Context) {
    OUTBOX.with(|slot| *slot.borrow_mut() = Some((failed, ctx)));
}

fn report(name: &str, message: String) {
    OUTBOX.with(|slot| {
        if let Some((failed, ctx)) = slot.borrow().as_ref() {
            if let Ok(mut q) = failed.lock() {
                q.push((name.to_string(), message));
            }
            ctx.request_repaint();
        }
    });
}

/// `PdfCraftApp::host_save`：把保存出的字节交给宿主写盘。
///
/// 上游的钩子是同步的 `fn`，而宿主的对话框是 Promise，所以这里发起一次后台任务并
/// 立刻返回 `Ok`（与上游网页版「下载已开始即算已保存」的语义一致）。写盘失败会
/// 通过 failed_inbox 报在界面上。
pub fn deliver(name: &str, bytes: &[u8], save_as: bool) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("需要一个文件名".into());
    }
    let request = save_request(name, &base64::engine::general_purpose::STANDARD.encode(bytes), save_as);
    let name = name.to_string();
    wasm_bindgen_futures::spawn_local(async move {
        match save_file(&request).await {
            Ok(Some(path)) => log::info!("saved {name} -> {path}"),
            // 用户在另存对话框里取消：上游会把它当成「没保存」，这里同步告知。
            Ok(None) => report(&name, "保存已取消".to_string()),
            Err(e) => report(&name, format!("保存失败：{e}")),
        }
    });
    Ok(())
}

fn save_request(name: &str, data: &str, force_pick: bool) -> JsValue {
    let request = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&request, &JsValue::from_str("name"), &JsValue::from_str(name));
    let _ = js_sys::Reflect::set(&request, &JsValue::from_str("dataBase64"), &JsValue::from_str(data));
    let _ = js_sys::Reflect::set(&request, &JsValue::from_str("forcePick"), &JsValue::from_bool(force_pick));
    request.into()
}

/// 返回落盘的真实路径；`None` = 用户在对话框里取消。
async fn save_file(request: &JsValue) -> Result<Option<String>, String> {
    let value = JsFuture::from(host_save_file(request.clone())).await.map_err(js_error)?;
    if value.is_null() || value.is_undefined() {
        return Ok(None);
    }
    Ok(field(&value, "path"))
}

fn field(value: &JsValue, key: &str) -> Option<String> {
    js_sys::Reflect::get(value, &JsValue::from_str(key)).ok()?.as_string()
}

fn js_error(e: JsValue) -> String {
    e.as_string().unwrap_or_else(|| format!("{e:?}"))
}
