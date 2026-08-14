# SerialMCP

串口 MCP 工具，供 Codex 通过 MCP stdio 调用，用于连接嵌入式开发板 UART 调试串口。

- 默认参数：`115200` 波特率、`8` 数据位、`1` 停止位、无校验位、无流控。

## MCP 配置

先将可执行文件 `serial-mcp` 放到某个环境变量目录下。

### Codex

编辑 `~/.codex/config.toml`，加入以下内容后重启 Codex：

```toml
[mcp_servers.serial]
type = "stdio"
command = "serial-mcp"
args = []
```

### Claude Code

编辑 `~/.claude.json`：

```json
{
  "mcpServers": {
    "serial": {
      "type": "stdio",
      "command": "serial-mcp",
      "args": []
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

- 进程通过 stdin/stdout 与 MCP 客户端通信，日志只写 stderr，不要手动交互运行。
- 一个 server 进程可同时打开多个 COM 口；`port` 省略时使用最后打开的“活动端口”。
- `serial_read` 在超时且无数据时会返回空 `data`，`timed_out` 为 `true`；读终端输出建议配合 `until: "line"`。
