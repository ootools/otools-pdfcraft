//! 上游 `pdfcraft-automation` 与宿主 MCP 契约之间的薄适配（见宿主 spec §5.4）。
//!
//! **全部方法都走上游 `McpServer` 的分发**，包括 tools / resources / prompts：
//! 上游 `McpServer::handle` 已经实现了 `tools/list`、`tools/call`、`resources/list`、
//! `resources/templates/list`、`resources/read`，并且工具的注解（readOnlyHint /
//! destructiveHint / idempotentHint / openWorldHint）也是它生成的。
//! 适配层再手写一份映射，只会在上游改注解时悄悄过期。
//!
//! 与 designcraft / vectorcraft 的差异：那两家的 `Server::new` 要求 `Box<dyn Backend>`，
//! 而 `pdfcraft` 的 `McpServer::new(Automation)` 直接持有具体类型，所以这里可以直接把
//! **整个 `McpServer`** 放进静态槽——连代理层都不需要。
//!
//! **有意不做按名字的 UI-only 过滤**：与另外几个 craft 插件同一条原则，
//! 失败交回上游给出的可读错误（AI 能据此改参数）。

use pdfcraft_automation::mcp::McpServer;
use pdfcraft_automation::Automation;
use serde_json::{json, Value};

/// 进程内**常驻**的上游 MCP 服务。
///
/// 上游 `McpServer` 持有 `Automation`（内含 Session），工具序列有状态
/// （doc_open → doc_info → doc_save 依赖同一个 Session）。每次调用新建会让第二次
/// 调用面对空会话，所以必须常驻；宿主热更新时走 `__otools_shutdown__` 重建。
static SERVER: std::sync::OnceLock<std::sync::Mutex<McpServer>> = std::sync::OnceLock::new();

fn server() -> &'static std::sync::Mutex<McpServer> {
    SERVER.get_or_init(|| std::sync::Mutex::new(McpServer::new(Automation::new())))
}

/// 走一次上游 MCP 分发，取 `result`。
///
/// 错误码（`-32602` 参数错、`-32002` 资源不存在）由上游给出，这里只把 message
/// 提出来交给宿主——resources 没有 `isError` 信封，必须变成 `Err`。
fn rpc(method: &str, params: Value) -> Result<Value, String> {
    let mut guard = server()
        .lock()
        .map_err(|_| "headless 会话状态已损坏，请重启 OTools".to_string())?;
    let message = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });
    let reply = guard
        .handle(&message)
        .ok_or_else(|| format!("上游对 {method} 没有应答"))?;
    if let Some(error) = reply.get("error") {
        let text = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("上游返回错误");
        return Err(text.to_string());
    }
    reply
        .get("result")
        .cloned()
        .ok_or_else(|| format!("{method} 的应答缺少 result"))
}

/// 宿主 `__otools_mcp_tools__` 的响应体（163 个工具，注解由上游生成）。
pub fn tools_payload() -> Value {
    match rpc("tools/list", json!({})) {
        Ok(value) => value,
        Err(error) => json!({ "tools": [], "error": error }),
    }
}

/// 宿主 `__otools_mcp_call__` 的响应体（`{content, isError, structuredContent?}`）。
pub fn call_payload(name: &str, arguments: &Value) -> Value {
    match rpc(
        "tools/call",
        json!({ "name": name, "arguments": arguments }),
    ) {
        Ok(value) => value,
        Err(error) => json!({
            "content": [{ "type": "text", "text": error }],
            "isError": true
        }),
    }
}

/// 宿主 `__otools_mcp_resources__` 的响应体。
///
/// 上游的资源表是**动态**的：每打开一份文档就多一个 `pdfcraft://doc/{id}/…`
/// 条目，所以必须真问上游，不能写死。
pub fn resources_payload() -> Result<Value, String> {
    rpc("resources/list", json!({}))
}

/// 宿主 `__otools_mcp_resource_templates__`（4 个模板：info / text / page text / page image）。
pub fn resource_templates_payload() -> Result<Value, String> {
    rpc("resources/templates/list", json!({}))
}

/// 宿主 `__otools_mcp_read_resource__`。
pub fn read_resource_payload(uri: &str) -> Result<Value, String> {
    rpc("resources/read", json!({ "uri": uri }))
}

/// 关掉会话（供宿主热更新前的停机握手调用）。
pub fn shutdown_automation() {
    if let Some(lock) = SERVER.get() {
        if let Ok(mut guard) = lock.lock() {
            *guard = McpServer::new(Automation::new());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn tools_payload_maps_upstream_tool_defs() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let value = tools_payload();
        let tools = value
            .get("tools")
            .and_then(|v| v.as_array())
            .expect("tools array");
        // 上游有 163 个工具（spec 里的关键约束）。
        assert!(
            tools.len() > 100,
            "expected the full upstream tool table, got {}",
            tools.len()
        );
        for tool in tools {
            assert!(
                tool.get("name").and_then(|v| v.as_str()).is_some(),
                "every tool needs a name: {tool}"
            );
            assert!(
                tool.get("inputSchema").is_some(),
                "every tool needs a schema: {tool}"
            );
        }
    }

    #[test]
    fn tools_payload_passes_through_read_only_and_destructive_flags() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let value = tools_payload();
        let tools = value["tools"].as_array().unwrap();
        let find = |name: &str| {
            tools
                .iter()
                .find(|t| t["name"] == name)
                .unwrap_or_else(|| panic!("{name} missing"))
        };
        // doc_info 是只读；doc_save 是 destructive（上游 .destructive()）。
        assert_eq!(find("doc_info")["annotations"]["readOnlyHint"], true);
        assert_eq!(find("doc_save")["annotations"]["destructiveHint"], true);
    }

    #[test]
    fn call_unknown_tool_returns_is_error() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let result = call_payload("definitely_not_a_tool", &json!({}));
        assert_eq!(result["isError"], true, "result: {result}");
    }

    #[test]
    fn call_read_only_tool_succeeds_headless() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // command_list 在 headless 下可用（无文档也能列出命令）。
        let result = call_payload("command_list", &json!({}));
        assert_eq!(result["isError"], false, "result: {result}");
        assert!(
            result.get("content").and_then(|v| v.as_array()).is_some(),
            "must return MCP content array: {result}"
        );
    }

    #[test]
    fn session_state_persists_across_calls() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // 防止「每次调用新建 Automation」回归：doc_list 必须看得到前面打开的文档。
        // 先造一个最小 PDF 再打开，避免依赖测试夹具。
        let fixture = minimal_pdf_path();
        let opened = call_payload("doc_open", &json!({ "path": fixture }));
        assert_eq!(
            opened["isError"], false,
            "doc_open should succeed: {opened}"
        );

        let listed = call_payload("doc_list", &json!({}));
        assert_eq!(
            listed["isError"], false,
            "doc_list must see the document opened in the previous call: {listed}"
        );
        let text = listed["content"][0]["text"].as_str().unwrap_or_default();
        assert!(
            text.contains("\"documents\""),
            "doc_list must return documents: {text}"
        );
    }

    /// 一份最小的合法 PDF（足以被 engine 打开），写到临时目录。
    fn minimal_pdf_path() -> String {
        let dir = std::env::temp_dir().join("otools-pdfcraft-native-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("minimal.pdf");
        // 最小可解析 PDF：一页空白，带正确的 xref。
        let pdf = "%PDF-1.4\n\
1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n\
2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n\
3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>\nendobj\n\
xref\n0 4\n0000000000 65535 f \n0000000009 00000 n \n0000000058 00000 n \n0000000115 00000 n \n\
trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n190\n%%EOF\n";
        std::fs::write(&path, pdf).unwrap();
        path.to_string_lossy().to_string()
    }
}
