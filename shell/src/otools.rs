//! OTools 文件服务：打开走 BrowserFiles，保存与另存通过宿主对话框和写盘 Promise。
//! 框架通过票号轮询 Paths/Cancelled/Failed；同步 write 路径异步提交并在界面报告失败。
//! JS 桥位于 shell/index.html 的 window.pdfcraftOtools。

use std::cell::RefCell;

use base64::Engine as _;
use craft::host::{Files, FileError, PickId, Picked, PickRequest, WriteIntent};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
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

    /// 宿主投递的「打开方式」：JS 侧已把文件读成字节攒好，这里一次取走。
    /// 元素形如 `{path, name, dataBase64}`（见 `shell/index.html` 的投递段）。
    #[wasm_bindgen(js_namespace = pdfcraftOtools, js_name = "takeDelivered")]
    fn take_delivered_raw() -> js_sys::Array;
}

/// 宿主投递来的一份文件（JS 侧已读成字节）。
pub struct Delivered {
    pub name: String,
    /// 真实落盘路径：给了它，「保存」才会写回原文件而不是弹另存对话框。
    pub path: Option<String>,
    pub bytes: Vec<u8>,
}

/// 取走宿主投递的文件（每帧调一次，取空即无事）。
///
/// 与保存相反的方向：宿主在系统里双击已绑定扩展名的文件 → 投递队列 → 本函数取走 →
/// 上游 `PdfCraftApp::open_bytes`。上游网页版的「打开」是 rfd（`<input type=file>`），
/// 外壳换不掉，但打开**字节**的入口是公开的，所以这条路不需要改上游。
pub fn drain_delivered() -> Vec<Delivered> {
    let raw = take_delivered_raw();
    let mut out = Vec::new();
    for item in raw.iter() {
        if !item.is_object() {
            continue;
        }
        let name = field(&item, "name").unwrap_or_default();
        if name.trim().is_empty() {
            continue;
        }
        let path = field(&item, "path").filter(|p| !p.trim().is_empty());
        let encoded = field(&item, "dataBase64").unwrap_or_default();
        let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
            log::warn!("delivered file {name} is not valid base64");
            continue;
        };
        out.push(Delivered { name, path, bytes });
    }
    out
}

thread_local! {
    /// 同步 write 提交后的异步错误通过 UI notice 显示。
    static OUTBOX: RefCell<Option<egui::Context>> = const { RefCell::new(None) };
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

/// 记住 UI 上下文，异步写盘失败时显示 notice。
pub fn install(ctx: egui::Context) {
    OUTBOX.with(|slot| *slot.borrow_mut() = Some(ctx));
}

fn report(name: &str, message: String) {
    OUTBOX.with(|slot| {
        if let Some(ctx) = slot.borrow().as_ref() {
            let _ = craft::ui::notices::NoticeCenter::new().post(
                ctx, &craft::ui::notices::PostedNotice::new(format!("{name}: {message}")), &[],
            );
            ctx.request_repaint();
        }
    });
}

/// Files::write 的宿主实现：把保存出的字节交给宿主写盘。
///
/// 上游的钩子是同步的 `fn`，而宿主的对话框是 Promise，所以这里发起一次后台任务并
/// 立刻返回 `Ok`（与上游网页版「下载已开始即算已保存」的语义一致）。写盘失败会
/// 通过 UI notice 报在界面上。
pub fn deliver(name: &str, bytes: &[u8], save_as: bool) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("需要一个文件名".into());
    }
    let request = save_request(name, &base64::engine::general_purpose::STANDARD.encode(bytes), save_as);
    if !save_as && (name.starts_with('/') || name.starts_with('\\') || name.as_bytes().get(1) == Some(&b':')) {
        let _ = js_sys::Reflect::set(&request, &JsValue::from_str("path"), &JsValue::from_str(name));
    }
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

/// Browser picking plus OTools asynchronous saves over the framework Files interface.
pub struct HostFiles {
    browser: craft::platform::browser_files::BrowserFiles,
    saves: Arc<Mutex<crate::requests::Requests<PickRequest, Picked>>>,
    ctx: egui::Context,
}

impl HostFiles {
    pub fn new(ctx: egui::Context) -> Self {
        let wake = ctx.clone();
        let browser = craft::platform::browser_files::BrowserFiles::new(move || wake.request_repaint())
            .page_saves(|name, bytes, save_as| available().then(|| deliver(name, bytes, save_as)));
        Self { browser, saves: Arc::default(), ctx }
    }
}

impl Files for HostFiles {
    fn pick(&self, request: PickRequest) -> PickId { self.browser.pick(request) }
    fn take_answer(&self, id: PickId) -> Option<Picked> {
        if id.0 & (1 << 63) == 0 { return self.browser.take_answer(id); }
        self.saves.lock().unwrap_or_else(|e| e.into_inner()).take_answer(id.0)
    }
    fn forget(&self, id: PickId) {
        if id.0 & (1 << 63) == 0 { self.browser.forget(id); }
        else { self.saves.lock().unwrap_or_else(|e| e.into_inner()).forget(id.0); }
    }
    fn pick_pending(&self) -> bool {
        self.browser.pick_pending() || self.saves.lock().unwrap_or_else(|e| e.into_inner()).pending().is_some()
    }
    fn read(&self, path: &Path) -> Result<Vec<u8>, FileError> { self.browser.read(path) }
    fn write(&self, path: &Path, bytes: &[u8], intent: WriteIntent) -> Result<bool, FileError> {
        if available() {
            deliver(&path.to_string_lossy(), bytes, matches!(intent, WriteIntent::SaveAs))
                .map(|_| true).map_err(FileError::Other)
        } else { self.browser.write(path, bytes, intent) }
    }
    fn last_folder(&self, purpose: &str) -> Option<PathBuf> { self.browser.last_folder(purpose) }
    fn save(&self, request: PickRequest, bytes: Vec<u8>) -> PickId {
        if !available() { return self.browser.save(request, bytes); }
        let id = PickId(self.saves.lock().unwrap_or_else(|e| e.into_inner()).issue(request.clone()));
        let name = request.file_name.as_deref().unwrap_or("document.pdf");
        let request = save_request(name, &base64::engine::general_purpose::STANDARD.encode(bytes), true);
        let state = self.saves.clone();
        let ctx = self.ctx.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let picked = match save_file(&request).await {
                Ok(Some(path)) => Picked::Paths(vec![PathBuf::from(path)]),
                Ok(None) => Picked::Cancelled,
                Err(error) => Picked::Failed(error),
            };
            state.lock().unwrap_or_else(|e| e.into_inner()).finish(id.0, picked);
            ctx.request_repaint();
        });
        id
    }
    fn can_pick(&self) -> bool { false }
    fn browser_saves(&self) -> bool { true }
}
