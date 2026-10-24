# PdfCraft · OTools 插件

把 [storytold/pdfcraft](https://github.com/storytold/pdfcraft)（开源、纯 Rust 重写的 Acrobat 替代品）
移植为 OTools 插件：上游的 UI 原封不动编译到 WebAssembly，外壳把「文件从哪来、存到哪去」
换成 OTools 宿主的对话框与磁盘读写。

- 插件目录：`plugins/otools-pdfcraft`（在 [ootools/OTools](https://github.com/ootools/OTools) 主仓里）
- 上游源码：**不入库**，见 `upstream.pin` + `scripts/fetch-upstream.mjs`

## MCP

插件通过宿主 MCP 网关暴露上游 `pdfcraft-automation` 的 170 个工具，以及上游的动态资源：

| 能力 | 内容 |
| --- | --- |
| tools | 170 个（`mcp.expose` 是默认勾选种子） |
| resources | `pdfcraft://document`、`pdfcraft://commands`，外加每份已打开文档的 `pdfcraft://doc/{id}/…`（**动态**） |
| resourceTemplates | 4 个：`doc/{doc}/info`、`doc/{doc}/text`、`doc/{doc}/page/{page}/text`、`doc/{doc}/page/{page}/image{?dpi}` |
| prompts | 无 |

适配层把**整个上游 `McpServer`** 放进静态槽：`McpServer::new(Automation)` 直接持有具体类型
（不像 designcraft / vectorcraft 的 `Server::new(Box<dyn Backend>)`），所以连代理层都不需要。
`tools/list` 也走上游，工具注解（`readOnlyHint` / `destructiveHint` / `idempotentHint` /
`openWorldHint`）由上游 `tool_json` 生成，不再手写映射。

> `Cargo.toml` 里的 `pdfcraft-automation` 必须开 `features = ["mcp"]`：默认关闭时
> `pdfcraft_automation::mcp` 整个模块不存在，资源表也就没了。

```bash
cargo test --release --manifest-path native/Cargo.toml
cargo build --release --manifest-path native/Cargo.toml
node scripts/smoke-mcp.mjs ..
```

## 上游版本

打包所用的上游提交。`upstream.pin` 是唯一真源，这张表用来回溯「哪个插件版本打的是哪份上游代码」，
每次发布都要在这里加一行。

| 插件版本 | 上游标签 | 上游提交 | 打包日期 |
| --- | --- | --- | --- |
| 0.1.5 | v0.9.0 | `645016addcfb9078c557f0189559ab47cdcee057` | 2026-10-24 |
| 0.1.4 | v0.9.0 | `645016addcfb9078c557f0189559ab47cdcee057` | 2026-10-21 |
| 0.1.3 | — | `9931329b9fae0b05373b4dc3ac7c58541f0891a4` | — |

## 构建

```bash
unset RUSTC_WRAPPER                 # 它会把 cargo 指向旧 rustc
node scripts/fetch-upstream.mjs     # 拉取 pinned 的上游到 upstream/
node scripts/build-wasm.mjs         # trunk build --release → dist/
```

需要 rustc >= 1.95 与 wasm32-unknown-unknown 目标（egui 0.36 的要求）。

详细约定见 [AGENTS.md](./AGENTS.md)。
