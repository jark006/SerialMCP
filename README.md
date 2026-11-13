# SerialMCP

让 AI Agent 通过 USB 串口操作开发板：读取启动日志、登录嵌入式 Linux、执行命令、收发单片机数据，以及传输文件。

支持 **Windows x64**，通过 HTTP MCP 接入客户端。默认地址为 `http://127.0.0.1:8765/mcp`。

## 安装

1. 从 [Releases](https://github.com/jark006/SerialMCP/releases) 下载 Windows x64 发布包，解压到固定目录，例如 `C:\Tools\SerialMCP`。
2. 安装 USB 转串口驱动，在设备管理器中确认 COM 口。核对开发板串口电平，连接 GND，并将转接器 TX 接开发板 RX、RX 接开发板 TX。
3. 在 PowerShell 中启动服务，保持窗口运行：

```powershell
cd C:\Tools\SerialMCP
.\serial-mcp.exe
```

## 接入 AI 客户端

在支持 HTTP MCP 的客户端中添加以下服务地址：

```text
http://127.0.0.1:8765/mcp
```

Codex CLI：

```powershell
codex mcp add serial --url http://127.0.0.1:8765/mcp
```

Claude Code：

```powershell
claude mcp add --transport http serial http://127.0.0.1:8765/mcp
```

添加后重新连接 MCP，让 Agent 枚举串口，确认能看到设备。添加 MCP 配置不会自动启动 SerialMCP，服务程序也需要保持运行。

默认地址仅监听本机。服务没有身份验证，多个客户端会共享串口连接；建议保持默认监听地址。

## 开始使用

告诉 Agent 要操作的设备、串口参数和任务。例如：

> 列出本机串口。开发板连接在 COM3，使用 115200、8N1、无流控。先读取现有输出，再判断是否需要登录 Linux。

> 在已经登录的开发板上执行 `uname -a`，然后查看 `/tmp` 的磁盘空间，报告命令退出码。

> 设备使用二进制协议。通过 COM3 发送 `AA 01 00 FF`，接收 4 字节响应，以 hex 展示。

> 将本机 `C:\build\app.bin` 上传到开发板 `/tmp`，使用 ZMODEM，完成后核对文件长度和校验值。

默认串口参数为 **115200、8 数据位、1 停止位、无校验、无流控**，应按设备实际配置调整。Linux、登录终端和 Bootloader 使用 `terminal` 模式；单片机原始字节协议使用 `binary` 模式。选择模式不会识别设备系统或完成登录。

文件传输支持 XMODEM、XMODEM-1K、YMODEM 和 ZMODEM。Linux 端默认使用可执行的 `lrz/lsz`；其他工具或 Bootloader 可以指定启动命令。本机文件路径指运行 SerialMCP 的 Windows 主机路径。

## 给 AI Agent 的快速约定

1. 先用 `serial_list_ports` 识别设备，再用 `serial_open` 打开正确端口。多设备时显式传入 `port`。
2. 登录和交互使用 `serial_run_command`；已登录 POSIX Shell 后，用 `serial_exec` 获取完成状态和退出码。
3. 二进制请求使用 `serial_exchange`。按设备协议指定响应长度、分隔符或静默窗口。
4. 超时、静默或取消等待不代表远端命令结束。不要自动重发有副作用的命令或整个文件。
5. 输出出现 `truncated` 时，跟随 `output_id` 和 `next_offset` 分页取回，不要为获取完整输出重跑命令。

完整工作流、文件传输示例和异常处理见 [Agent 使用指南](docs/agent-guide.md)，参数及结果含义见 [工具手册](docs/tools.md)。

## 常见问题

| 现象 | 处理方式 |
| --- | --- |
| 客户端连接不到 MCP | 确认服务窗口仍在运行，地址包含 `/mcp`，然后重新连接客户端 |
| 找不到或打不开串口 | 检查驱动、设备管理器和接线，关闭占用端口的串口助手 |
| 输出乱码或没有响应 | 核对波特率、校验、流控和电平，让 Agent 先读取已有输出再判断状态 |
| 文件未出现在预期目录 | Y/ZMODEM 接收目录由远端接收程序的工作目录决定，上传时应在启动命令中指定目录 |
| 长输出显示不全 | 让 Agent 使用 `serial_get_output` 分页，保留原请求的输出标识 |

## 升级和更多文档

退出旧服务后替换程序，再启动服务并重新连接 MCP。可用 `.\serial-mcp.exe --version` 查看版本；端口被占用时，可用 `.\serial-mcp.exe --bind 127.0.0.1:9000` 更换监听地址，并同步修改客户端配置。

| 文档 | 内容 |
| --- | --- |
| [Agent 使用指南](docs/agent-guide.md) | 连接、登录、命令、二进制交互和文件传输工作流 |
| [工具手册](docs/tools.md) | 全部工具、参数默认值、输出分页和错误含义 |
| [升级说明](docs/migration.md) | 0.3 到 0.4 及后续版本的兼容变化 |
| [开发指南](docs/development.md) | 源码构建、修改和打包 |
| [实现与协议](docs/architecture.md) | 并发、缓冲、Shell 包装和文件协议的实现边界 |
| [测试与验收](docs/testing.md) | 自动测试和可选硬件测试的执行方法 |
| [Linux 实机验收](docs/testing-linux.md) | 已验证设备、覆盖范围和报告索引 |

采用 [MIT 许可证](LICENSE)。
