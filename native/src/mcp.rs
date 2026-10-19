//! 上游 `pdfcraft-automation` 与宿主 MCP 契约之间的薄适配（见宿主 spec §5.4）。
//!
//! 与 designcraft / vectorcraft 的差异（**不是照抄**）：
//! - 工具表函数是 `tools()`，返回 `Vec<ToolDef>`（不是 `Vec<Value>`），
//!   需要自行映射成 `{ name, title, description, inputSchema, annotations }`；
//!   `ToolDef` 带原生 `read_only` / `destructive`，直接映射到 MCP 注解。
//! - 执行入口是 `Automation::call(name, args) -> Result<Vec<Content>, ToolError>`，
//!   结果要自己拼成 MCP 的 content / isError。
//! - 上游有 163 个工具（spec §10.4 的关键约束）。
//!
//! **有意不做按名字的 UI-only 过滤**：与另外两个 craft 插件同一条原则，
//! 失败交回上游给出的可读错误（AI 能据此改参数）。

use base64::Engine as _;
use pdfcraft_automation::{tools, Automation, Content, ToolError};
use serde_json::{json, Value};

/// 宿主 `__otools_mcp_tools__` 的响应体。
pub fn tools_payload() -> Value {
    let tools: Vec<Value> = tools()
        .into_iter()
        .map(|t| {
            json!({
                "name": t.name,
                "title": t.title,
                "description": t.description,
                "inputSchema": t.input_schema,
                "annotations": {
                    "readOnlyHint": t.read_only,
                    "destructiveHint": t.destructive,
                }
            })
        })
        .collect();
    json!({ "tools": tools })
}

/// 进程内**常驻**的 headless 会话。
///
/// 上游 `Automation` 持有 Session，工具序列有状态（doc_open → doc_info → doc_save
/// 依赖同一个 Session）。每次调用新建会让第二次调用面对空会话。
static AUTOMATION: std::sync::OnceLock<std::sync::Mutex<Automation>> = std::sync::OnceLock::new();

fn automation() -> &'static std::sync::Mutex<Automation> {
    AUTOMATION.get_or_init(|| std::sync::Mutex::new(Automation::new()))
}

/// 关掉会话（供宿主热更新前的停机握手调用）。
pub fn shutdown_automation() {
    if let Some(lock) = AUTOMATION.get() {
        if let Ok(mut backend) = lock.lock() {
            *backend = Automation::new();
        }
    }
}

/// 宿主 `__otools_mcp_call__` 的响应体。
pub fn call_payload(name: &str, arguments: &Value) -> Value {
    let mut guard = match automation().lock() {
        Ok(guard) => guard,
        Err(_) => {
            return json!({
                "content": [{ "type": "text", "text": "headless 会话状态已损坏，请重启 OTools" }],
                "isError": true
            })
        }
    };
    match guard.call(name, arguments) {
        Ok(content) => call_result(content),
        Err(error) => error_result(&error),
    }
}

/// `Vec<Content>` → MCP content 数组（与上游 `mcp.rs::call_result` 同形）。
fn call_result(content: Vec<Content>) -> Value {
    let mut structured = None;
    let blocks: Vec<Value> = content
        .into_iter()
        .map(|c| match c {
            Content::Json(v) => {
                let text = serde_json::to_string_pretty(&v).unwrap_or_default();
                if v.is_object() && structured.is_none() {
                    structured = Some(v);
                }
                json!({ "type": "text", "text": text })
            }
            Content::Png { data, .. } => json!({
                "type": "image",
                "mimeType": "image/png",
                "data": base64::engine::general_purpose::STANDARD.encode(data)
            }),
        })
        .collect();
    let mut result = json!({ "content": blocks, "isError": false });
    if let Some(s) = structured {
        result["structuredContent"] = s;
    }
    result
}

/// 工具错误 → `isError: true` 的可读结果（与上游一致，让 AI 能自行纠正）。
fn error_result(error: &ToolError) -> Value {
    json!({
        "content": [{ "type": "text", "text": error.to_string() }],
        "isError": true
    })
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
