# 工具使用手册

本文说明当前工具行为、默认值和结果含义；完整参数 Schema 由 MCP `tools/list` 提供。操作流程见 [Agent 指南](agent-guide.md)，版本差异见 [升级说明](migration.md)，协议与实现细节见 [实现与协议](architecture.md)。

## 工具索引

| 场景 | 工具 |
| --- | --- |
| 发现与状态 | `serial_list_ports`、`serial_status` |
| 打开、选择、配置和恢复 | `serial_open`、`serial_select`、`serial_configure`、`serial_reconnect`、`serial_close` |
| 读写与一次请求应答 | `serial_read`、`serial_write`、`serial_exchange` |
| 终端、Shell 和按键 | `serial_run_command`、`serial_exec`、`serial_send_key` |
| 停止等待、取回长输出 | `serial_cancel`、`serial_get_output` |
| 文件传输 | `serial_check_modem_tools`、`serial_send_file`、`serial_receive_file` |
| 缓冲与硬件控制 | `serial_flush`、`serial_set_control` |

## 通用约定

- `serial_open` 和 `serial_select` 必填 `port`；其他支持该参数的工具在省略或为 `null` 时使用活动端口。成功打开、重连或选择会更新活动端口，所有客户端共享选择。多设备应显式传入端口；枚举、全局状态和输出分页不使用 `port`。
- `response_mode` 默认 `compact`，正常结果只返回一份 JSON 文本；`detailed` 增加 `structuredContent`，数据工具还提供 `hex`、`raw_data`（如适用）。枚举结果为 `{"ports":[...]}`。
- `output_limit` 是每个展示字段的 UTF-8 字节上限，默认 8192，范围 4～1048576。长输出可用 `serial_get_output` 获取 `data`、`hex` 或 `raw_data`；偏移以 UTF-8 字节计，跟随 `next_offset`。
- `timeout_ms` 范围 1～600000 毫秒。读取/命令的接收期限不包含之前的发送阶段，驱动调用可能使实际耗时超过期限。文件传输中表示单次协议响应的读取超时，不是整个文件的总期限。
- `max_bytes` 范围 1～1048576，限制实际接收量。触及上限后停止读取，后续数据留待下次；它不是展示长度。
- hex 支持空白和一个可选的开头 `0x`，例如 `0x00 03 FF`。文本为 UTF-8，非法 UTF-8 显示会替换字符，精确字节应使用 hex。
- 工具不自动重试发送。驱动异常可能发生在部分字节已发出之后，不能凭失败推断目标未执行。
- ANSI 清理不等同于终端仿真，不模拟光标定位或全屏应用。

compact 正常结果的 `content[0].text` 是一份 JSON 文本，串口内容在其 `data` 字段中。detailed 还提供对象形式的 `structuredContent`。调用方应检查 JSON-RPC 错误、工具 `isError` 和操作自身的完成状态；非零 Shell 退出码是有效执行结果，不等同于 MCP 调用错误。

## 连接与状态

`serial_list_ports` 枚举设备及 USB 身份。`serial_open` 必须提供 `port`，其他参数默认：

| 参数 | 默认 | 可选值 |
| --- | --- | --- |
| `profile` | `terminal` | `terminal`、`binary` |
| `baud_rate` | 115200 | 1～4000000，实际取决于驱动 |
| `data_bits` | 8 | 5、6、7、8 |
| `stop_bits` | 1 | 1、2 |
| `parity` | `none` | `none`、`odd`、`even` |
| `flow_control` | `none` | `none`、`software`、`hardware` |
| `timeout_ms` | 1000 | 驱动基础超时，1～600000 |

`serial_configure` 至少修改一项。设置按顺序应用，中途失败时部分设置可能已生效，应查询状态。只修改 profile 不改变波特率、换行或驱动缓冲区。

`serial_status` 重新枚举并校准空闲端口：`available`（存在未打开）、`open`、`faulted`（最近操作失败）、`disconnected`（句柄释放、参数保留）。忙端口附带 `busy`、`operation`、`cancellable`，硬件信息为操作前快照。

`serial_reconnect` 按保存的 USB 序列号和 VID/PID 恢复；序列号唯一时可跟随 COM 号变化。发现不同身份默认拒绝，确认替换设备才用 `force: true`。没有序列号时无法保证物理身份。重连保留参数与模式，清空旧连接的软件未读字节，不重放操作。

`serial_select` 选择已跟踪端口。`serial_close` 关闭并遗忘它，关闭活动端口后需要选择或打开另一端口。

## 读取、写入与应答

`serial_write` 必填 `data`，`encoding` 默认 terminal=text、binary=hex。`line_ending` 默认 `none`，可选 `lf`、`cr`、`crlf`；terminal 模式不会自动补换行。

`serial_read` 默认等待 1000 毫秒、最多 4096 字节。`until` 可选 `timeout`（默认）、`line`、`sequence`。sequence 必须提供 `terminator`，其编码默认同 `encoding`，可用 `terminator_encoding` 独立指定。匹配发生在原始字节上，支持跨数据块；包含分隔符，保留尾部字节。

line 匹配第一个 CR 或 LF，同批 CRLF 一并返回；分批到达的 CR、LF 可能分次返回，需要完整 CRLF 时使用 sequence。可选 `idle_timeout_ms` 是收到首字节后的静默窗口，必须不大于整体期限。

```json
{"name":"serial_read","arguments":{"encoding":"hex","until":"sequence","terminator":"00 FF","timeout_ms":3000}}
```

```json
{"name":"serial_write","arguments":{"encoding":"hex","data":"03"}}
```

`serial_exchange` 合并写入和读取，`encoding` 控制请求，`response_encoding` 默认同请求。支持 `terminator`、`terminator_encoding`、`idle_timeout_ms`，默认无静默窗口。固定帧长使用 `max_bytes`。`clear_input` 默认 false，仅在明确要丢弃旧输入时设 true。工具不解释设备帧地址或校验 CRC。

| `stop_reason` | 含义 |
| --- | --- |
| `sequence` / `line` | 匹配目标，`matched: true` |
| `max_bytes` | 达到接收上限，固定帧长可据此判断收齐 |
| `timeout` | 到整体期限，可能有部分数据 |
| `idle` | 收到数据后达到静默窗口 |
| `cancelled` | 收到取消请求，返回已收数据 |
| `end_of_stream` | 驱动返回零字节，不代表设备命令完成 |
| `error` | 读取后出现设备/驱动错误；保留已收数据和 error，工具 isError=true |
| `completed` | 仅 exec：解析到完成标记和退出码 |

## 终端与 Shell

`serial_run_command` 用于登录、交互程序、Bootloader，必填 `command`，默认追加 LF，期限 5000 毫秒、静默窗口 300 毫秒、接收上限 65536 字节。指定 `until` 后等待精确序列并忽略静默窗口。`strip_ansi` 默认 true。

`clear_input` 默认 false，保留旧提示符和启动日志。true 清空软件未读字节及驱动输入。旧提示符可能使 until 过早匹配，已经登录的 POSIX Shell 应优先用 `serial_exec`。

`serial_exec` 必填 `command`，默认期限 5000 毫秒、上限 65536 字节。它在子 Shell 中执行命令，返回 `completed`、`exit_code`、`remote_may_be_running`。只有标记及退出码解析成功才完成；非零退出码也是有效命令结果。未知退出码在 compact 中省略。超时、取消或上限不会终止远端程序；后台任务即使 Shell 已完成也可能继续运行。

要求目标已登录、支持 POSIX printf、子 Shell 和 eval。不能用于登录口令、U-Boot、REPL 或全屏程序。目录、环境变量不跨调用保留，写成 `cd /tmp && command`。不要单独运行等待文件协议的 rz 等交互命令，应使用文件传输工具。

exec 的标准输入为 EOF，长短命令一致。`read`、`cat` 等未显式指定输入的命令会读到 EOF。管道、文件输入和 here-document 等显式重定向正常生效；交互式串口输入使用 run_command/read/write。

exec 支持长命令，接收上限包含终端回显，因此较长输入可能需要提高 `max_bytes`。原始 write/exchange/run_command 不自动改写用户输入。

调用时的 PATH、已导出变量和工作目录传入命令；内部求值与完成标记不受同名函数或别名接管。命令对 stdout/stderr 的重定向及非零退出仍可取得完成状态。

`serial_send_key` 支持 `ctrl_c`、`ctrl_d`、`ctrl_z`、`ctrl_l`、`enter`（CR）、`escape`、`tab`、`backspace`（DEL）和四个方向键。目标终端设置决定是否解释它们。binary 模式用 `serial_write` 显式发等价 hex。

## 取消与并发

同端口独占，冲突请求立即以 `isError: true` 返回 busy，不发送字节。不同端口独立。打开/关闭/重连还有生命周期互斥，避免 COM 迁移覆盖另一连接，冲突时可等连接操作结束再试。

`serial_cancel` 取消 read、run_command、exec、exchange 或 modem probe 的等待。读取工具返回已收数据；probe 未完成时返回取消错误，不把缺失结果报告成“未安装”。若操作已先完成，取消可能不再生效。取消不打断驱动写入，不取消文件传输，不向远端发送 Ctrl+C。等原请求结束后再发送控制键。

## 输出分页

`serial_get_output` 使用原结果的 `output_id`，不读取串口。`field` 可选 data（默认）、raw_data、hex；`offset` 默认 0，`limit` 默认 4096，范围 4～1048576，单位均为 UTF-8 字节。

```json
{"name":"serial_get_output","arguments":{"output_id":"output-1","field":"data","offset":0,"limit":4096}}
```

跟随 `next_offset` 读取，直到 `has_more: false`。页长还受 `output_limit` 限制，返回偏移只推进实际展示的字节，不要自行用 offset + limit 推算。非 UTF-8 字符边界偏移会被拒绝。

缓存最多保留最近 16 个被截断结果，合计最多 16 MiB，重启清空；失效后不能重新分页。`output_note` 会说明结果未能保留的情况。不要为取回长输出重跑有副作用的命令。

## 文件传输

send/receive_file 在同次独占操作中启动目标程序并完成协议。默认 zmodem，支持 xmodem、xmodem-1k、ymodem。单文件最多 **64 MiB**；完整接收后落盘，本地已存在的路径会被拒绝，父目录必须已存在。

`local_path` 是运行服务的 Windows 路径。自动启动远端发送程序时 `remote_path` 必填；自动启动 XMODEM 接收程序时也必填。Y/ZMODEM 上传可省略远端文件名，默认采用本地文件名；远端目录由目标程序工作目录决定，可用 `command` 指定。

自动命令保护 `-leading.bin`、`-`、`--help` 等以减号开头的相对路径，保持文件名含义。自定义 command 的引用和选项由调用方负责。路径及启动命令不允许 NUL；文件内容仍可包含任意字节。

| 协议 | 默认接收命令（上传到板） | 默认发送命令（从板下载） |
| --- | --- | --- |
| XMODEM | `lrz -X -c -y 'path'` | `lsz -X 'path'` |
| XMODEM-1K | `lrz -X -c -y 'path'` | `lsz -X -k 'path'` |
| YMODEM | `lrz --ymodem -y` | `lsz --ymodem 'path'` |
| ZMODEM | `lrz -y` | `lsz 'path'` |

先用 `serial_check_modem_tools` 探测 PATH 中可执行的 lrz/lsz/rz/sz/rx/sx/rb/sb 文件；存在但不可执行时返回 false。**默认使用 lrz/lsz，不自动回退**；需要时传 `command`。binary 模式必须指定命令或 `start_command: false`。若目标已等待协议握手，用 false 保留未读握手字节。

自动生成的 Shell 命令会在传输前后同步远端 Shell。成功时 `shell_ready: true`、`remote_exit_code: 0`。自定义 POSIX Shell 命令需加 `shell_command: true`；Bootloader 命令保持 false（自定义命令的默认值）。仅协议完成时返回 `shell_ready: false`，下一次交互前应自行读取确认终端状态。`start_command: false` 不能与 `shell_command: true` 同用。

```json
{"name":"serial_send_file","arguments":{"local_path":"C:\\build\\app.bin","remote_path":"app.bin","command":"cd /tmp && lrz -y","shell_command":true}}
```

Y/ZMODEM 的接收目录由接收程序的工作目录决定，上传时显式进入目标目录；ZMODEM 传输的是 basename，不能将 remote_path 的目录部分当作 cd。

Bootloader 自定义命令示例：

```json
{"name":"serial_send_file","arguments":{"protocol":"xmodem-1k","local_path":"C:\\build\\firmware.bin","command":"loadx"}}
```

默认每次协议响应超时 10000 毫秒，最多连续 16 次超时/重试；客户端请求期限也需足够长。失败不重放整个文件。Y/ZMODEM 携带大小，精确等长；XMODEM 无长度，接收保留末块 0x1A 填充，调用方应按已知大小处理。目前只支持单文件。

X/YMODEM 的 NAK 或超时可能触发当前数据包重传，无关应答不会触发包重传。整个文件不会自动重放。

请勿在自定义下载命令中传入多个源文件。Y/ZMODEM 检测到第二个文件后拒绝整个接收，不保存第一个文件；拒绝时取消对端协议，启用 Shell 同步时继续收取远端退出状态。

启用 Shell 同步的传输失败时，如果取得对应的非零退出标记，错误包含远端退出码和诊断，例如 Permission denied。协议取消或远端命令失败不等同于硬件断开；真实驱动异常仍会更新连接状态。Shell 文件启动命令使用原来的串口标准输入，不受命令长度影响。

## 缓冲区与故障处理

`serial_flush.direction` 可为 input、output、both（默认），含义是**丢弃**数据，不是等待发送完成；input 同时清软件未读字节。`serial_set_control` 支持 dtr、rts、break_ms（0～10000），至少一项。控制线及 break 可触发部分开发板复位。

无法打开时检查驱动及串口助手占用。乱码先核对参数，字节设备使用 hex。不响应时先读现有输出，判断是否需要 CR、登录或退出交互程序，避免重复发送有副作用的命令。

参数错误返回 JSON-RPC -32602；有效请求的设备/文件错误为 `isError: true`。timeout、idle、max_bytes 通常是带有部分结果的正常返回，调用方必须检查结束原因。

工具错误的 content 文本可能直接是英文错误消息，并可附带 `structuredContent.error`；不要假设错误文本都能作为 JSON 解析。
