# AGENTS.md

本文件为后续在此仓库工作的编码代理提供项目上下文与协作约定。请先阅读 `README.md`，那里有面向用户的完整使用说明。

## 项目概览

- `serial-mcp` 是一个 Windows x64 串口 MCP stdio 服务，供 Codex/Claude Code 等客户端调用，用于操作嵌入式开发板的 UART 调试串口。
- 技术栈：Rust 2024，依赖 `serde`、`serde_json`、`serialport`。
- 进程通过 stdin 读取 JSON-RPC 请求，通过 stdout 返回响应；诊断日志只能写 stderr，禁止手动交互运行。
- 目标平台为 Windows；`.cargo/config.toml` 已启用 `crt-static`，release 产物不依赖 `VCRUNTIME140.dll`。

## 常用命令

在仓库根目录执行：

```powershell
cargo fmt --all -- --check
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
```

- 发布产物：`target\release\serial-mcp.exe`
- 提交前至少保证 `cargo fmt --check` 与 `cargo test` 通过。

## 代码结构

- `src/main.rs`
  - 程序入口：解析 `--help`/`-h`，然后进入 stdin 的 JSON-RPC 行循环。
  - 解析失败时返回 `-32700 parse error`；stdout 写入失败时退出。
- `src/mcp.rs`
  - MCP/JSON-RPC 协议层：`handle_message` 分发 `initialize`、`ping`、`tools/list`、`tools/call` 等消息。
  - 定义全部 9 个 MCP 工具的参数结构、默认值、输入校验和 `inputSchema`。
  - 包含协议层单元测试，例如初始化响应、通知无响应、工具列表、hex 解析。
- `src/serial.rs`
  - 串口业务层：`SerialHub` 管理打开端口、活动端口和串口参数。
  - 包含纯函数与参数解析单元测试，例如端口规范化、VID/PID 格式化、串口参数解析。
  - 真实串口硬件操作无法在单测中覆盖，只测试可脱离硬件验证的逻辑。

## 关键约定

- 默认串口参数定义在 `src/serial.rs`：
  - `baud_rate: 115200`、`data_bits: 8`、`stop_bits: 1`、`parity: "none"`、`flow_control: "none"`、`timeout_ms: 1000`。
- 端口名通过 `normalize_port` 做 `trim` + 大写；例如 `" com3 "` 归一化为 `"COM3"`。
- 多数工具的 `port` 可省略，省略时使用 `SerialHub.active`（最近成功打开的端口）。
- MCP 参数校验在 `mcp.rs`：
  - 非法参数返回 JSON-RPC `-32602 invalid params`。
  - 合法参数但硬件操作失败时，`tools/call` 返回 `isError: true` 的工具结果。
- 编码只支持 `text` 与 `hex`；`serial_write` 的 hex 输入可含空格和可选 `0x` 前缀。
- `serial_read` 同时返回 `data`（按 encoding 解码）和 `hex`（大写十六进制、空格分隔）。

## 修改时的检查清单

- 新增 MCP 工具时，同步更新：
  - `mcp.rs` 中的 `tool_names` 与 `tool_definitions`。
  - `call_tool` 分发逻辑、参数结构体与校验函数。
  - 相应单元测试和 `README.md` 的工具表。
- 修改默认值或参数范围时，保持 `mcp.rs` 的校验逻辑、`serial.rs` 的常量和 `inputSchema` 一致。
- 保持现有错误信息为英文；面向用户的中文说明放在 `README.md` 或最终回复中。
- 本仓库偏好最小化、聚焦的变更，不引入无关重构或额外依赖。
