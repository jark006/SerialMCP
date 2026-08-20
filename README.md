# SerialMCP

SerialMCP 是面向 Windows x64 的 MCP Streamable HTTP 串口服务，用于让 Codex、Claude Code 等 Agent 与嵌入式 Linux UART 终端交互。它同时提供底层串口读写工具，以及更适合 Agent 的原子化 Shell 命令工作流。

服务默认监听 `127.0.0.1:8765`，MCP 端点为 `POST /mcp`。所有诊断日志只写入 stderr。

## 启动服务

```powershell
serial-mcp.exe
serial-mcp.exe --bind 127.0.0.1:9000
serial-mcp.exe --help
```

默认只绑定回环地址。服务没有身份验证；使用非回环地址时应通过防火墙限制访问。

## MCP 配置

Codex CLI：

```powershell
codex mcp add serial --url http://127.0.0.1:8765/mcp
```

也可以添加到 `~/.codex/config.toml`：

```toml
[mcp_servers.serial]
url = "http://127.0.0.1:8765/mcp"
```

Claude Code：

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

## 推荐的 Agent 工作流

1. 调用 `serial_list_ports`，结合 `port`、`vid`、`pid` 和 `serial_number` 识别开发板。
2. 调用 `serial_open`，指定 COM 口和开发板串口参数。
3. 执行 Linux Shell 命令时优先使用 `serial_run_command`。它会在一次原子操作中发送命令并收集完整响应。
4. 读取启动日志、处理登录流程或操作二进制协议时，使用 `serial_read` 和 `serial_write`。
5. USB 拔插后调用 `serial_status`；设备重新出现后调用 `serial_reconnect`。服务绝不会自动重试失败的写入。

所有 MCP 客户端共享同一个进程级串口状态。省略 `port` 时使用当前活动端口。关闭活动端口会清除选择，不会把后续命令静默发送到另一块开发板。

## MCP 工具

| 工具 | 作用 |
| --- | --- |
| `serial_list_ports` | 实时枚举 COM 口及 USB VID/PID、序列号、厂商和产品名。 |
| `serial_status` | 重新枚举硬件，报告 `available`、`open`、`faulted` 或 `disconnected` 状态。 |
| `serial_open` | 打开串口；默认 `115200 8N1`、无流控。 |
| `serial_reconnect` | 使用保存的参数重新连接。USB 序列号唯一时可跟随 COM 号变化；设备身份改变时需显式 `force: true`。 |
| `serial_close` | 关闭并遗忘打开、故障或断开的串口记录。 |
| `serial_configure` | 修改一个或多个串口参数；至少需要提供一个参数。 |
| `serial_run_command` | 原子化发送终端命令并收集响应，推荐用于嵌入式 Linux Shell。 |
| `serial_check_modem_tools` | 在开发板 shell 中检测 `lrz`、`lsz` 及 `rz/sz/rx/sx/rb/sb` 兼容命令。 |
| `serial_send_file` | 通过 XMODEM、XMODEM-1K、YMODEM 或 ZMODEM 将 Windows 本地文件发送到开发板。 |
| `serial_receive_file` | 通过 XMODEM、XMODEM-1K、YMODEM 或 ZMODEM 将开发板文件保存到 Windows 本地。 |
| `serial_write` | 写入文本或 hex 字节，可追加 `none`、`lf`、`cr` 或 `crlf` 行结束符。 |
| `serial_read` | 按整体超时、行结束符、精确序列或字节上限读取文本/hex 数据。 |
| `serial_flush` | 清空输入、输出或全部驱动缓冲区。 |
| `serial_set_control` | 设置 DTR/RTS，或发送 serial break 执行开发板复位序列。 |

### 执行 Shell 命令

`serial_run_command` 的主要参数：

- `command`：必填的 UTF-8 终端输入。
- `line_ending`：默认 `lf`；可按目标终端改为 `cr`、`crlf` 或 `none`。
- `timeout_ms`：包含等待首字节在内的整体期限，默认 `5000`。
- `idle_timeout_ms`：未指定 `until` 时，收到数据后的静默结束窗口，默认 `300`。
- `until`：可选的精确提示符或标记，例如 `"# "`、`"$ "` 或 `"login:"`。提供后会等待该序列或整体超时。
- `clear_input`：默认 `true`，防止旧提示符被误判为本次命令完成；需要保留启动/登录输出时设为 `false`。
- `strip_ansi`：默认 `true`；清理后的终端文本放在 `data`，原始文本和字节仍保存在 `raw_data` 与 `hex`。

示例：

```json
{"name":"serial_run_command","arguments":{"command":"uname -a","until":"# "}}
{"name":"serial_run_command","arguments":{"command":"dmesg | tail -20","idle_timeout_ms":500}}
```

### X/Y/ZMODEM 文件传输

`serial_send_file` 和 `serial_receive_file` 会在同一次原子串口操作中启动开发板端程序并完成整个协议会话。不要先用 `serial_run_command` 单独运行 `rz` 或 `sz`：该命令会等待协议数据，并阻塞后续串口请求。

可先调用 `serial_check_modem_tools` 检测开发板是否安装了 `lrz/lsz`。检测命令通过 `command -v` 完成，返回每个命令的布尔值；`lrz/lsz` 通常是 `rz/sz` 的增强版或别名。文件传输默认使用 `lrz/lsz`；如果检测结果显示不存在，请通过 `command` 参数显式指定目标上的 `rz/sz/rx/sx/rb/sb` 兼容命令。

默认协议是 `zmodem`，也可以选择 `xmodem`、`xmodem-1k` 或 `ymodem`。默认启动命令如下：

| 协议 | Windows → 开发板 | 开发板 → Windows |
| --- | --- | --- |
| XMODEM | `lrz -X -c -y` / `rx` | `lsz -X` / `sx` |
| XMODEM-1K | `lrz -X -c -y` / `rx` | `lsz -X -k` / `sx -k` |
| YMODEM | `lrz --ymodem -y` / `rb` | `lsz --ymodem` / `sb` |
| ZMODEM | `lrz -y` / `rz -y` | `lsz` / `sz` |

路径含义：

- `local_path` 始终是运行 `serial-mcp.exe` 的 Windows 主机路径，不是 MCP 客户端所在机器的路径。
- 发送时，`remote_path` 是目标文件名；XMODEM 自动启动时必须提供。Y/ZMODEM 省略它会使用 `local_path` 的文件名。
- 接收并自动启动远端命令时必须提供 `remote_path`。
- 可通过 `command` 覆盖默认命令，例如 `cd /tmp && rz -y`。如果开发板端程序已经在等待协议握手，设置 `start_command: false`；此时不会清空现有输入，以保留握手字节。
- `timeout_ms` 是每次协议响应的等待时间，默认 `10000`。连续 16 次超时后传输会取消。

示例：

```json
{"name":"serial_send_file","arguments":{"local_path":"C:\\build\\firmware.bin","remote_path":"firmware.bin"}}
{"name":"serial_receive_file","arguments":{"remote_path":"/tmp/log.tar.gz","local_path":"C:\\logs\\log.tar.gz","protocol":"zmodem"}}
{"name":"serial_send_file","arguments":{"local_path":"C:\\build\\u-boot.bin","remote_path":"u-boot.bin","protocol":"xmodem-1k","command":"loadx"}}
```

YMODEM 和 ZMODEM 会传输文件大小，因此接收结果与源文件精确等长。XMODEM 协议没有文件大小元数据，接收文件会保留最后一个 128/1024 字节块中的 `0x1A` 填充；若目标命令另有长度信息，应在传输后按已知长度截取。工具使用 CRC、块序号校验和 NAK 重传；传输失败时不会自动重放整个文件。

### 底层读取

`serial_read` 返回 `data`、`hex`、`bytes_read`、`stop_reason`、`matched` 和 `timed_out`。`stop_reason` 为 `timeout`、`idle`、`line`、`sequence` 或 `max_bytes` 之一。

```json
{"name":"serial_read","arguments":{"timeout_ms":3000,"until":"line"}}
{"name":"serial_read","arguments":{"timeout_ms":5000,"until":"sequence","terminator":"login:"}}
{"name":"serial_write","arguments":{"data":"root","line_ending":"lf"}}
```

读取使用整体期限，不会因为数据持续零星到达而无限延长。非 UTF-8 字节会在 `data` 中替换显示，但 `hex` 始终保留原始值。

## 热插拔状态

服务不运行后台设备监听器。`serial_status` 会执行一次实时枚举并校准所有已跟踪端口：

- `available`：设备存在，但当前进程没有打开；
- `open`：句柄已打开，最近操作正常；
- `faulted`：设备仍可枚举，但最近一次串口操作失败；
- `disconnected`：设备消失或驱动明确返回无设备错误，旧句柄已经释放。

设备断开时会保留活动端口选择和串口参数，因此重新插入后可以直接调用 `serial_reconnect`。重连会优先通过 USB 序列号以及 VID/PID 确认设备，并在序列号唯一时跟随新的 COM 号。如果原 COM 号被另一设备占用，默认拒绝连接。任何可能已经部分发送的命令或字节都不会被自动重放。

所有串口操作通过同一个共享状态串行执行。长时间读取或文件传输会暂时阻塞其他串口请求，因此应根据命令和文件大小设置合理的 `timeout_ms`。

## 默认串口参数

- `baud_rate`: `115200`
- `data_bits`: `8`
- `stop_bits`: `1`
- `parity`: `none`
- `flow_control`: `none`
- `timeout_ms`: `1000`

## 构建与验证

项目使用 Rust 2024、Axum、Tokio、Serde、`serialport` 和 `zmodem2`；X/YMODEM 协议核心由项目内实现。`.cargo/config.toml` 启用了 MSVC 静态 CRT；release 配置启用 LTO、单 codegen unit 和体积优化。

```powershell
cargo fmt --all -- --check
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
```

发布产物为 `target\release\serial-mcp.exe`。
