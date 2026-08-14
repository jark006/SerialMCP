# SerialMCP

串口 MCP 工具，通过 MCP Streamable HTTP 协议连接 Codex、Claude Code 等客户端，用于操作嵌入式开发板 UART 调试串口。

- 默认参数：`115200` 波特率、`8` 数据位、`1` 停止位、无校验位、无流控。
- 默认地址：`http://127.0.0.1:8765/mcp`。

## 启动服务

先启动 `serial-mcp.exe`，默认只监听本机回环地址。需要修改监听地址时使用：

```powershell
serial-mcp --bind 127.0.0.1:9000
```

可通过 `serial-mcp --help` 查看启动参数。服务需持续运行，MCP 客户端不会再负责启动该进程。

## MCP 配置

### Codex

可以通过 Codex CLI 添加：

```powershell
codex mcp add serial --url http://127.0.0.1:8765/mcp
```

也可以编辑 `~/.codex/config.toml`，加入以下内容后重启 Codex：

```toml
[mcp_servers.serial]
url = "http://127.0.0.1:8765/mcp"
```

### Claude Code

编辑 `~/.claude.json`：

```json
{
  "mcpServers": {
    "serial": {
      "type": "http",
      "url": "http://127.0.0.1:8765/mcp"
    }
  }
}
```

## 提供的 MCP 工具

| 工具 | 作用 |
| --- | --- |
| `serial_list_ports` | 枚举 Windows COM 口及 USB VID/PID、序列号、厂商、产品名 |
| `serial_open` | 打开 COM 口，可指定波特率、数据位、停止位、校验位、流控、读超时 |
| `serial_close` | 关闭 COM 口；不传 `port` 时关闭当前活动端口 |
| `serial_configure` | 修改已打开端口的串口参数或读超时 |
| `serial_write` | 写入文本或 hex 字节；默认文本，可选追加换行 |
| `serial_read` | 读取字节，直到超时、读到行结束或达到 `max_bytes` |
| `serial_status` | 查看活动端口、已打开端口及全部 COM 口状态 |
| `serial_flush` | 清空输入/输出缓冲区 |
| `serial_set_control` | 设置 DTR/RTS，或发送指定毫秒的 serial break，常用于复位开发板 |

`serial_open` 默认参数：

- `baud_rate`: `115200`
- `data_bits`: `8`
- `stop_bits`: `1`
- `parity`: `"none"`
- `flow_control`: `"none"`
- `timeout_ms`: `1000`

## 使用示例

配置完成后，可以在 Codex 里直接说：

- “列出可用串口”
- “打开 COM3，115200 8N1”
- “向串口发送 ls 并换行”
- “读取串口一行输出”
- “把波特率改成 921600”
- “给开发板发一个 50ms break 复位”

对应工具调用示例：

```json
{"name":"serial_list_ports","arguments":{}}
{"name":"serial_open","arguments":{"port":"COM3"}}
{"name":"serial_write","arguments":{"data":"ls\n"}}
{"name":"serial_read","arguments":{"until":"line","timeout_ms":3000}}
{"name":"serial_configure","arguments":{"baud_rate":921600}}
{"name":"serial_set_control","arguments":{"break_ms":50}}
```

## 构建

本机安装 Rust stable 后：

```powershell
cargo build --release
```

产物在 `target\release\serial-mcp.exe`。项目已通过 `.cargo/config.toml` 配置 Windows MSVC 静态 CRT，默认产物不依赖 `VCRUNTIME140.dll`。

验证：

```powershell
cargo fmt --all -- --check
cargo test
cargo clippy --all-targets -- -D warnings
```

## 说明

- 服务实现 MCP Streamable HTTP，客户端通过 `POST /mcp` 发送消息；当前不需要服务端推送，因此 `GET /mcp` 返回 `405 Method Not Allowed`。
- 默认监听 `127.0.0.1` 且不提供身份验证。仅在受信网络中使用非回环 `--bind` 地址，并通过防火墙限制访问。
- 为防止 DNS rebinding，带 `Origin` 请求头的浏览器请求会被拒绝；Codex、Claude Code 等原生 MCP 客户端不受影响。
- 服务不分配 MCP session ID；所有连接共享同一个串口状态和活动端口。
- 一个 server 进程可同时打开多个 COM 口；`port` 省略时使用最后打开的“活动端口”。
- `serial_read` 在超时且无数据时会返回空 `data`，`timed_out` 为 `true`；读终端输出建议配合 `until: "line"`。
