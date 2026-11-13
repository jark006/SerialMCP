# Agent 使用指南

本文面向通过 MCP 操作串口设备的 AI Agent。安装与客户端接入见 [README](../README.md)，完整参数和结果字段见 [工具手册](tools.md)。示例使用 MCP 工具调用的 `name` / `arguments` 表示法，按说明分别调用。

## 识别并连接设备

先枚举端口，结合用户指定的 COM 号、USB VID/PID、序列号和产品名识别设备；不要仅凭“第一个串口”选择目标。

```json
{"name":"serial_list_ports","arguments":{}}
```

用 `serial_status` 检查已有连接和正在进行的操作。需要新建连接时打开端口，参数按设备说明填写：

```json
{"name":"serial_open","arguments":{"port":"COM3","profile":"terminal","baud_rate":115200,"data_bits":8,"stop_bits":1,"parity":"none","flow_control":"none"}}
```

`terminal` 用于 Linux、登录终端、Bootloader 和交互程序；`binary` 用于原始字节协议。模式不会识别设备系统。多端口或多客户端时，在支持 `port` 的调用中显式指定端口，避免依赖共享的活动端口。

## Linux 登录与命令

连接后先读取已有输出，判断当前是启动日志、登录提示符还是已登录 Shell：

```json
{"name":"serial_read","arguments":{"port":"COM3","timeout_ms":1500,"idle_timeout_ms":150}}
```

没有输出时，可按需要发送一次 Enter，再读取。已观察到 `login:` 且确认目标用户名后，可使用：

```json
{"name":"serial_run_command","arguments":{"port":"COM3","command":"root","until":"Password:"}}
```

提示符应使用设备实际输出。等到口令提示后再输入用户提供的口令，不把口令写入测试脚本或报告。登录和 Bootloader 交互使用 `serial_run_command`，不能用 `serial_exec` 发送用户名或口令。

登录 POSIX Shell 后，用 `serial_exec` 执行非交互命令：

```json
{"name":"serial_exec","arguments":{"port":"COM3","command":"uname -a; id"}}
```

检查 `completed` 和 `exit_code`。`completed: true` 表示命令已返回，`exit_code: 0` 才表示命令成功；非零退出码也是有效的执行结果。提示符、回显和一段静默都不能代替完成状态。

每次 exec 使用子 Shell。需要切换目录或设置环境变量时，将它们与目标命令写在同一次调用中：

```json
{"name":"serial_exec","arguments":{"port":"COM3","command":"cd /tmp && ls -la","timeout_ms":10000}}
```

exec 的默认标准输入为 EOF。管道、文件重定向和 here-document 可正常使用；等待串口键盘输入的程序应改用交互工具。

## 原始字节设备

打开 `binary` 模式，按设备协议发送请求，不添加 Shell 命令或换行。已打开的同一端口用 `serial_configure` 切换 profile；下面示例用于新连接：

```json
{"name":"serial_open","arguments":{"port":"COM3","profile":"binary","baud_rate":115200}}
```

已知响应为 4 字节时：

```json
{"name":"serial_exchange","arguments":{"port":"COM3","data":"AA 01 00 FF","max_bytes":4,"timeout_ms":1000}}
```

按分隔符接收时：

```json
{"name":"serial_exchange","arguments":{"port":"COM3","data":"AA 10 00","terminator":"0D 0A","timeout_ms":2000}}
```

binary 默认使用 hex。按 `stop_reason` 判断是否达到长度或分隔符；超时返回可能只是部分帧。匹配后的字节留给后续读取，默认不要清输入。

## Linux 文件传输

先在已登录 Shell 中调用 `serial_check_modem_tools`，确认工具实际可执行。默认上传使用 `lrz`，下载使用 `lsz`；设备仅有 `rz/sz` 时显式指定命令。

`local_path` 属于运行 SerialMCP 的 Windows 主机。上传前检查本机源文件，下载选择不存在的新目标文件，并保证父目录已存在。

Y/ZMODEM 的接收目录取决于远端程序的工作目录，`remote_path` 的目录部分不能代替 `cd`。例如上传到 `/tmp`：

```json
{"name":"serial_send_file","arguments":{"port":"COM3","protocol":"zmodem","local_path":"C:\\build\\app.bin","remote_path":"app.bin","command":"cd /tmp && lrz -y","shell_command":true}}
```

下载文件：

```json
{"name":"serial_receive_file","arguments":{"port":"COM3","remote_path":"/tmp/app.bin","local_path":"C:\\logs\\app-received.bin"}}
```

自动生成的 Linux 命令会同步远端 Shell；自定义 Linux 命令要设置 `shell_command: true`。检查 `shell_ready` 和 `remote_exit_code`，按任务要求核对长度、校验值或实际内容。

XMODEM 没有文件长度字段，下载结果可能包含尾部 `0x1A` 填充。按已知原始长度核对，不要无条件删除尾部 `0x1A`，因为它也可能是实际数据。

不要单独用 exec 启动等待协议的 `lrz/rz`，而应使用文件工具的启动参数。对已在等待握手的程序，才使用 `start_command: false`；Bootloader 和手动协议见工具手册。

## 长输出

默认使用 compact 响应，只有需要原始字节、ANSI 输出或结构化结果时才请求 detailed。`output_limit` 控制展示量，`max_bytes` 控制实际接收量。

出现 `truncated: true` 时保留原 `output_id`，从偏移 0 分页取回：

```json
{"name":"serial_get_output","arguments":{"output_id":"output-1","field":"data","offset":0,"limit":4096}}
```

后续使用返回的 `next_offset`，直到 `has_more: false`。偏移单位是 UTF-8 字节，不要按字符数推算。分页不会读取串口或重跑远端命令；缓存会淘汰，重启服务后也会失效。

## 超时、取消与恢复

| 状态 | 后续操作 |
| --- | --- |
| exec 超时、取消或达到接收上限 | 保留部分输出和 `remote_may_be_running`，继续读取或检查状态，不自动重发命令 |
| 端口 busy | 等原操作结束；被拒绝的请求未发送，不在后台排队 |
| 需要取消本地等待 | 调用 `serial_cancel`，从原请求取回部分数据，并等原请求返回 |
| 需要中断远端交互程序 | 本地请求返回后，按设备终端行为发送 `ctrl_c`，随后读取并确认状态 |
| USB 重插或驱动断开 | 先查 `serial_status`，再按保存的设备身份调用 `serial_reconnect` |
| 文件传输失败 | 保存错误及已完成的状态，先确认对端已退出，再决定后续操作，不自动重放整个文件 |

`serial_cancel` 不发送 Ctrl+C，也不取消文件传输。DTR/RTS 和 break 可能复位设备，按已明确的硬件控制方案使用。`serial_flush` 会丢弃缓冲数据，不能用它等待数据发送完成。
