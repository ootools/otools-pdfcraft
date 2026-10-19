# AGENTS.md —— otools-pdfcraft 移植约定

给后续接手这个仓库的 agent（和人）的规则。核心一句话：**`upstream/` 一行都不要改**，
所有移植代码都放在 `shell/`（前端外壳）与 `native/`（宿主 MCP 网关的 headless 适配层），
这样上游同步永远干净。

## 铁律

1. **`upstream/` 保持上游原样**。它是 `storytold/pdfcraft` 在 `upstream.pin`
   里那个 commit 的浅拉取结果，**不在 git 里**（见 `scripts/fetch-upstream.mjs`）。
   任何「顺手改一下上游」的念头都会在下次同步时丢失或变成冲突。
2. `shell/src/` 里凡是标明「上游副本」的文件，不要就地改；要改行为就改
   `shell/src/otools.rs` 或 `shell/src/main.rs`。
3. **`shell/` 不引入 native cdylib**。前端侧宿主已注入
   `tools_webview_read_file` / `tools_webview_write_file` 与文件对话框，够用；
   再加 FFI 只是多一份动态库要跟着分发。
   **例外（已确立）**：`native/` 是为宿主 MCP 网关暴露 headless 能力的独立适配层
   （见下「native 适配层」一节），它不是前端外壳的替代，也不参与 WASM 构建。
4. **改 `shell/Cargo.toml` 的上游依赖版本时，对齐 `upstream/Cargo.toml` 的
   `[workspace.dependencies]`**（egui/eframe 目前是 `0.36` 这一档）。

## 上手前必须知道的三件事

- **rustc 版本**：上游 `rust-version = "1.95"`（egui 0.36 用了 1.95 才稳定的
  `Vec::push_mut`）。本机 1.94 会在 `epaint` 上直接 `E0658`。
  `scripts/build-wasm.mjs` 会检查并提示。本机环境变量 `RUSTC_WRAPPER=sccache`
  指向 PATH 里的旧 rustc，会让 cargo 用错工具链——构建前 `unset RUSTC_WRAPPER`。
- **git 拉取优先走 SSH**：HTTPS 直连 github.com 在本机会偶发超时（21s 连接失败），
  `fetch-upstream.mjs` 的策略是「SSH 优先，回落 HTTPS」。
- **trunk 首次运行会下载 wasm-bindgen-cli 与 wasm-opt**（GitHub release），
  国内网络可能要直连；CI 里已配 `CARGO_NET_GIT_FETCH_WITH_CLI`。

## 上游管理：pin + 按需拉取（不是 subtree / submodule）

上游源码**不在**本仓库里，只有 `upstream.pin` 记录了仓库与精确的 40 位 commit：

```json
{ "upstream": "storytold/pdfcraft", "ssh": "…", "https": "…", "ref": "main", "commit": "…" }
```

- **本地/CI 取源码**：`node scripts/fetch-upstream.mjs`（缺失才拉，已对齐就跳过；
  `--force` 丢弃本地改动重拉）。GitHub 允许按任意可达 commit 做 `--depth 1` 拉取。
- **同步上游更新**：改 `upstream.pin` 的 `commit` 为新的完整 SHA，重新拉取，
  然后 diff `upstream/apps/pdfcraft-web/src/*.rs` 与 `shell/src/*.rs`。
- **为什么不用 subtree**：单个快照 16~38 MB、五个合计约 150 MB，且每次同步只增不减，
  还会把上游几千个文件混进自己的树（干扰 grep / IDE 搜索）。pin 到 SHA 反而更可复现。

## 移植层的地图

| 文件 | 作用 |
| --- | --- |
| `shell/index.html` | 唯一的 JS：`window.pdfcraftOtools`（available/theme/openFiles/saveFile） |
| `shell/src/otools.rs` | 桥：`pick_open` / `pick_save` / `deliver` / 主题 |
| `shell/src/main.rs` | 外壳：eframe 起应用、`?cpu/?webgl` 等 URL 开关 |
| `native/` | 宿主 MCP 网关的 headless 适配层（独立 workspace，见下节）。产物 `lib/{macOS.dylib,Windows.dll,Linux.so}` 不进 git |
| `upstream/` | （不在 git 里）上游源码，都是 path 依赖直接复用 |

## native 适配层（宿主 MCP 网关）

`native/` 是把**上游现成的 headless 后端**（`pdfcraft-automation`）编成动态库的薄胶水，
让外部 AI 工具经宿主的大 MCP 端点操作本插件。它不是前端外壳，**不参与 WASM 构建**，
也与 `shell/` 互不依赖。

- **独立 workspace**（`native/Cargo.toml` 里有 `[workspace]`）：并进上游 workspace 会让
  构建把 UI 依赖（eframe/wgpu）也拉进来。
- **上游源码不在 git 里**：`native/` 用 path 依赖直接吃 `../upstream/crates/automation`，
  所以构建前必须先 `node scripts/fetch-upstream.mjs`。
- **`[patch.crates-io]` 必须手抄一份**：上游 workspace 的 patch 不会跟着 path 依赖带过来，
  少了它会退到 crates.io 的未打补丁版本。上游更新后要同步这里。
- **进程内常驻会话**：`Automation`（`new()`） 存在 dylib 的 `OnceLock` 里，工具序列才有状态
  （`native/src/mcp.rs` 的 `session_state_persists_across_calls` 用例守着这条）。
- **两个宿主 method**（宿主 spec §5.4 冻结的契约）：
  `__otools_mcp_tools__` → `tools()`（返回 `Vec<ToolDef>`，带原生 `read_only`/`destructive`）；`__otools_mcp_call__` → `Automation::call(name, args)`。
- **不做按工具名的 UI-only 过滤**：原样透传上游工具表，headless 下用不了的工具
  由上游在调用点返回可读错误（有「同名工具可用性相反」「可用性取决于参数」两种情况，
  按名字硬编码过滤会误杀）。
- **产物命名是宿主约定**：`lib/macOS.dylib` / `lib/Windows.dll` / `lib/Linux.so`
  （宿主 `native_plugins/core.rs` 的 `platform_lib_name()`），由 `scripts/pack-plugin.mjs`
  打进 `.oplg`；二进制**不进 git**（`.gitignore` 已忽略）。
- **构建/测试**：`cargo test --release --manifest-path native/Cargo.toml`。
  多平台产物由 `.github/workflows/release-plugin.yml` 的 `build-native`/`merge-native`
  两个 job 出（macOS 双架构 lipo 成 universal）。

## 常见任务

- **构建 dist/** → `node scripts/build-wasm.mjs`（会先自动 take upstream）。
- **本地试跑** → `scripts/deploy-to-otools.ps1` 同步到 otools 仓库的 `plugins/otools-pdfcraft`。
- **改 OTools 侧的元信息**（名称/图标/关键词）→ `plugin.json`。

## 不要做的事

- 不要把 `upstream/` 提交进 git（它是构建中间件，不是源码）。
- 不要给 `upstream/` 里的 crate 打 patch、加 `cfg`、写 TODO。
- 不要在本仓库跑 `cargo build` 而不管 target：外壳是 wasm-only 的，
  宿主（x86_64）下 `main()` 只会打印一行提示，别把它当编译报错。
