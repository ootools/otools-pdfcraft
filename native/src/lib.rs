//! PdfCraft 的 OTools 插件 native 层。
//!
//! 把上游 `pdfcraft-automation` 的 headless 自动化层（进程内 Session，无窗口）编成
//! 宿主可加载的动态库，经宿主 MCP 网关对外暴露工具（见宿主 spec §5.4、§10.2）。
//!
//! 编译成两种形态：
//! - `cdylib`：插件市场里以 `lib/macOS.dylib` 等形式被宿主 `libloading` 加载，
//!   入口是 `otools_plugin_invoke` / `otools_plugin_free`；
//! - `rlib`：给未来的独立 APP 壳直接引用。
//!
//! FFI 边界一律走 [`guard_invoke`]：插件里的 panic 不能跨 DLL 边界 unwind，
//! 否则宿主整个进程会被 `abort()` 掉。
//!
//! **为什么这里内联 FFI 守卫、不依赖主仓的 `ot-plugin-ffi`**：本仓是公开仓，
//! 而 `ootools/OTools` 主仓是私有仓，CI 拉不到；仓内既有先例（`otools-vm` 公开
//! native 插件仓）同样内联自己的守卫。两者逻辑一致，主仓那份是给主仓内插件用的。

pub mod mcp;

use serde_json::{json, Value};

fn dispatch(method: &str, payload: &Value) -> Result<Value, String> {
    match method {
        // SP1 冻结的两个 method。
        "__otools_mcp_tools__" => Ok(mcp::tools_payload()),
        "__otools_mcp_call__" => {
            let name = payload
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if name.trim().is_empty() {
                return Err("缺少工具名 name".to_string());
            }
            let arguments = payload
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            Ok(mcp::call_payload(name, &arguments))
        }
        // 宿主热更新前的停机握手：释放常驻 headless 会话。
        "__otools_shutdown__" => {
            mcp::shutdown_automation();
            Ok(json!({ "ok": true, "data": "otools-pdfcraft-native headless 会话已释放" }))
        }
        other => Err(format!("Unknown method: {other}")),
    }
}

// ---------------------------------------------------------------- FFI

/// 在 FFI 边界内捕获 panic，返回宿主可解析的 JSON（与 `otools-vm` 同形）。
fn guard_invoke<F>(
    input_ptr: *const u8,
    input_len: usize,
    output_len: *mut usize,
    logic: F,
) -> *mut u8
where
    F: FnOnce(&[u8]) -> Result<Value, String>,
{
    if input_ptr.is_null() || output_len.is_null() {
        return std::ptr::null_mut();
    }

    // SAFETY: 宿主保证 (input_ptr, input_len) 是可读区间。
    let input = unsafe { std::slice::from_raw_parts(input_ptr, input_len) };

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| logic(input)));

    let response = match outcome {
        Ok(Ok(value)) => {
            if value.get("ok").is_some() {
                value
            } else {
                json!({ "ok": true, "data": value })
            }
        }
        Ok(Err(error)) => json!({ "ok": false, "error": error }),
        Err(payload) => {
            let message = payload
                .downcast_ref::<&str>()
                .map(|text| (*text).to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_string());
            eprintln!("[pdfcraft-native] 插件内部 panic: {message}");
            json!({ "ok": false, "error": format!("插件内部错误: {message}"), "panic": true })
        }
    };

    let mut bytes = serde_json::to_vec(&response)
        .unwrap_or_else(|_| br#"{"ok":false,"error":"serialize failed"}"#.to_vec());
    if bytes.is_empty() {
        bytes.extend_from_slice(b"{}");
    }
    let len = bytes.len();
    unsafe {
        *output_len = len;
    }
    let ptr = bytes.as_mut_ptr();
    std::mem::forget(bytes);
    ptr
}

fn invoke_impl(input: &[u8]) -> Result<Value, String> {
    let parsed: Value =
        serde_json::from_slice(input).map_err(|error| format!("请求解析失败: {error}"))?;
    let method = parsed
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let payload = parsed.get("payload").cloned().unwrap_or(Value::Null);
    dispatch(method, &payload)
}

#[no_mangle]
pub extern "C" fn otools_plugin_invoke(
    input_ptr: *const u8,
    input_len: usize,
    output_len: *mut usize,
) -> *mut u8 {
    guard_invoke(input_ptr, input_len, output_len, invoke_impl)
}

#[no_mangle]
pub extern "C" fn otools_plugin_free(ptr: *mut u8, len: usize) {
    if ptr.is_null() || len == 0 {
        return;
    }
    unsafe {
        drop(Vec::from_raw_parts(ptr, len, len));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_rejects_missing_tool_name() {
        let error = dispatch("__otools_mcp_call__", &json!({})).unwrap_err();
        assert!(error.contains("name"), "got {error}");
    }

    #[test]
    fn dispatch_reports_unknown_method() {
        let error = dispatch("nope", &json!({})).unwrap_err();
        assert!(error.contains("Unknown method"), "got {error}");
    }

    #[test]
    fn invoke_impl_parses_method_and_payload() {
        let request = serde_json::to_vec(&json!({
            "method": "nope",
            "payload": { "a": 1 }
        }))
        .unwrap();
        let error = invoke_impl(&request).unwrap_err();
        assert!(error.contains("Unknown method"), "got {error}");
    }

    #[test]
    fn invoke_impl_rejects_malformed_json() {
        let error = invoke_impl(b"not json").unwrap_err();
        assert!(error.contains("请求解析失败"), "got {error}");
    }

    #[test]
    fn shutdown_is_accepted() {
        let result = dispatch("__otools_shutdown__", &json!({})).unwrap();
        assert_eq!(result["ok"], true);
    }
}
