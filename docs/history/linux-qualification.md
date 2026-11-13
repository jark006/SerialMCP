# Linux 开发板历史验收记录

本文保留各轮实测、故障复现与报告线索。文中的版本、检查数、工具权限和脚本行为属于对应测试阶段；当前结果见 [Linux 实机验收](../testing-linux.md)，复测方法见 [测试与验收](../testing.md)，当前工具行为见 [工具手册](../tools.md)。本地 target 报告不随仓库或发布包分发。

2026-11-13 使用真实 USB UART 连接测试，波特率 115200、8N1、无流控。目标为 Ingenic MIPS Linux 4.4.94，交互 Shell 为 BusyBox sh，文件工具为 lrzsz 0.12.20。通过会话中配置的 MCP 完成登录和初测，再用独立服务验证修正版，避免替换正在运行的 exe。登录口令不保存在脚本或报告中。

## 验收内容

`scripts/test_linux_board.py` 对已经登录的 POSIX Shell 执行 30 项集成检查：

- Shell 完成标记、中文/引号、无末尾换行、非零退出码、超过静默窗口的延迟输出。
- 8100 字节 UTF-8 输出截断及 180 行完整分页比对。
- 超时返回后继续收取输出；长请求期间 ping/status 保持可用，同端口 busy 写入不发送。
- 取消等待准确报告未完成，随后恢复 Shell；不假定发送 Ctrl+C 就一定终止目标进程。
- 四种文件协议双向传输、传输完成后立即执行下一条命令、目录隔离、串口重连。
- 恢复测试前的 PATH/cwd 并清理临时目录。原始 lrz/lsz 文件权限保持不变。

测试载荷包含所有 256 种字节值，长度 4099，SHA-256 为 `ed364c9d1fb40422180e962a3e45a07b812ab8da9b901ac85b05512bde19bb2d`。

| 协议 | 上传原始字节 | 下载字节 | 校验 |
| --- | ---: | ---: | --- |
| ZMODEM | 4099 | 4099 | 完全一致，板端 MD5 一致 |
| YMODEM | 4099 | 4099 | 完全一致，板端 MD5 一致 |
| XMODEM | 4099 | 4224 | 前 4099 字节一致，尾部全为 0x1A |
| XMODEM-1K | 4099 | 4224 | 前 4099 字节一致，尾部全为 0x1A |

原始报告为本地 `target/linux-board-report.json`，包含具体程序路径、版本、SHA-256 和检查项。复测命令见 [测试与验收](../testing.md)。

## 实测发现并修正的问题

0.4.1 检查 modem 工具是否为可执行文件，避免 command-v 对仅存在的文件返回成功。该板的 `/system/bin/lrz`、`lsz` 原始权限为 0600，测试在唯一 /tmp 目录创建可执行副本，不改系统分区文件。

Shell 文件传输在进入协议前等待不会被回显伪造的启动标记。ZMODEM 上传进一步等待对端真实头，避免程序尚未关闭终端回显时发出的 ZRQINIT 被回送。传输后等待远端命令完成标记，再释放端口，防止连续操作的数据被前一个程序吞掉。自定义 Bootloader 命令仍可使用原始模式。

ZMODEM 接收兼容 lrzsz 的高位换行 0x8A，处理分批 OO 结束握手，保留后续提示符。内存会话增加逐字节分片覆盖，驱动循环在解析器需要更多数据时继续读取，避免重复提交同一未完成片段。

## 验收边界

这些结论适用于上述设备和软件组合，不代表其他 Bootloader、不同串口参数或大型文件已验证。本次没有操作 DTR/RTS、发送 break、复位开发板或模拟物理断电热插拔。单片机二进制交互的全字节回环测试与本次 Linux Shell 验收分别执行。

## 新会话中的边界复测

同日通过用户重启后的 0.4.1 MCP 直接执行四种协议各八种长度的往返，共 32 组（64 次文件调用）。长度为 0、1、127、128、129、1024、1025、65537 字节，覆盖空文件、块大小边界，以及 XMODEM 128 字节块的序号回绕。全部在 Windows 本地逐字节比对通过；XMODEM/XMODEM-1K 的额外尾部仅为 0x1A。

另验证自定义 shell_command=true、含空格/单引号/中文的文件名、本地目标文件存在时拒绝覆盖、hex 分隔符读取后保留尾部、多行命令和非零退出码，以及接收上限不误报 Shell 完成。原始记录为 target/board-session/mcp-session-report.json，记录当前用户启动程序的 SHA-256。

异常复测发现：板端 lrz 无执行权限时，0.4.1 只报告协议超时。0.4.2 在有匹配的远端退出标记时保留错误原因，该板实际返回 `code 2: ... Permission denied`（不能假设所有 Shell 都返回 126），并保持设备状态为 open。对应回归已纳入脚本；在该板的完整流程现为 31 项（可执行 lrz 场景仍为 30 项）。修正版报告为 target/linux-board-regression-0.4.2.json。

## 重启后恢复与 0.4.3 验收

用户多次重启开发板并将 `/system/bin/lrz`、`lsz` 恢复为 0755 后，先核对现有传输已结束，再重连 COM3。开发板仍在已登录 Shell，工具探测确认系统 lrz/lsz 可执行。后续验收直接使用原程序，不再依赖副本，也不修改系统文件权限。

新脚本 test_linux_recovery.py 覆盖 21 项检查：900、1800、4096、16000、64000 字符命令，3000 UTF-8 字节的中文/引号/反斜杠输入，四种协议下载不存在文件后的错误与 Shell 恢复，真实传输中断，以及四协议 start_command=false 手动握手往返。长输出全部分页后与输入逐字节比对；64000 字符测试提高 max_bytes 以容纳终端回显。

本轮发现并修复：交互式 Shell 的单行及累计解析限制、长输入积压回显时的 UART 丢字节、ZMODEM 下载启动请求被 Shell 回显，以及 peer cancel 被误判为硬件断开。长命令通过短行写入 here-document，非交互子 Shell 重组后执行；按行和波特率控制发送节奏，并收取回显。所有字节仍留给结果读取，短暂静默不用于判定命令完成。

传输中断测试先查询 BusyBox timeout 帮助，该板语法为 `timeout -t 1 PROG`；确认实际生成小于原文件的部分文件后才计为通过。协议取消和远端错误保持连接，真实驱动错误仍保留断开状态。四种手动传输的 1025 字节载荷本地比对一致，XMODEM 额外字节仅为 0x1A。

ZMODEM 上传将确认窗口限制为每 KiB 一次，减少接收程序中途退出后仍在发送的数据量。中断载荷使用可打印的固定内容；完整字节值由常规文件往返验收覆盖。手动启动 ZMODEM 的测试会等到真实头开始，确保目标已进入协议，单独的 Shell 回显换行不能证明它已就绪。

恢复报告为 target/linux-recovery-0.4.3.json；系统工具的完整常规回归报告为 target/linux-board-regression-0.4.3.json。两者记录对应 release 二进制 SHA-256，清理唯一 /tmp 测试目录，保留开始测试前的工作目录。

## 多文件拒绝、落盘失败与缓存验收

用户启动 0.4.3 后，通过当前 MCP 补测发现：ZMODEM 检测到第二个文件时只改变了本地状态，没有发送线上的 CAN 序列；对端 lsz 继续等待，下一条 Shell 命令被吞掉。0.4.4 显式发送取消序列后再收集远端退出，协议拒绝原因也不会被远端退出码覆盖。内存协议测试使用真实 Sender 状态机发送两个文件，断言对端收到 CAN，而不只是检查本地错误。

新脚本 test_linux_qualification.py 在 Windows 上执行 23 项实机检查：Y/ZMODEM 拒绝批传输、无本地部分文件、无需手动 Ctrl+C 即可继续执行命令；Windows 非法目标文件名使协议完成后的落盘失败，随后串口继续使用；已有目标既不覆盖也不运行远端命令；错误后下一次单文件下载完整成功。

输出验证覆盖最小 4 字节分页中的中文和 emoji、拒绝非 UTF-8 边界偏移、detailed 文本与结构化结果一致、保留的 data/raw_data/hex 分页后完全一致，以及第 17 个截断结果淘汰最旧缓存。另验证 binary 模式拒绝隐式 Shell 后切回 terminal，以及关闭重开串口仍保留远端工作目录。

本轮使用系统 lrz/lsz，权限保持 0755。修正版的资格验收与完整常规回归分别记录在 target/linux-qualification-0.4.4.json 和 target/linux-board-regression-0.4.4.json；报告包含实际 release 二进制 SHA-256，所有唯一 /tmp 测试目录均须清理。

## 标准输入与接收竞态验收

通过用户启动的 0.4.4 MCP 发现：短 exec 命令继承串口终端，长命令继承包装脚本输入，read 等命令的行为随长度变化。更长的文件传输启动命令同样可能失去终端输入。0.4.5 将 exec 的默认 stdin 设为 EOF；文件启动命令使用保留的输入描述符，保持串口语义。两种包装均验证短命令和超过 128 字节的命令，显式管道与 here-document 不受影响。

扩展 qualification 脚本在 Windows 上共 41 项。长 Y/ZMODEM 启动命令先断言 stdin 为终端，然后完成 1025 字节的双向传输与逐字节比较；短长 exec 的 read 均立即返回 EOF，显式输入都能准确取回。

接收竞态通过同一服务的并发 HTTP 请求与本地文件操作复现：远端命令启动后、保存前创建同名目标，或删除空目标目录。保存阶段准确报错，后来创建的文件保持原内容；随后 Shell 命令正常。测试确认远端启动标记文件存在且返回的是保存错误，不能仅依赖请求 busy 状态推断预检已通过。

验收报告为 target/linux-qualification-0.4.5.json 和 target/linux-board-regression-0.4.5.json，记录对应 release SHA-256；前者 41 项，后者 30 项。所有数据仅写唯一测试目录，系统 lrz/lsz 权限保持 0755。

## 控制字符、重复调用与文件名边界

用户启动 0.4.5 后，终端颜色/退格、中文和 emoji 退格、OSC/DCS 清理、类似完成标记的普通输出、NUL/非法 UTF-8 在 raw hex 中完整保留等检查均通过。20 次连续调用的输出和退出码准确；超时请求的迟到完成标记不干扰下一请求，带副作用的超时命令没有重放；两秒静默也不会提前结束 exec。

新发现的问题是文件路径即使经过 Shell 引用，仍可能被 lrz/lsz 当作选项。0.4.6 为以减号开头的相对路径添加 ./ 前缀，文件名保持原样。四种协议分别验证 -leading.bin、-、--help 的上传和下载，逐字节比对数据，XMODEM 额外字节仅为 0x1A。元数据无法表示的 NUL 路径与命令在调用前拒绝；文件内容中的 NUL 不受影响。

扩展后的 Windows qualification 共 74 项，包含上一轮标准输入、落盘竞态、缓存和会话恢复检查。资格验收与 30 项常规回归分别记录在 target/linux-qualification-0.4.6.json 和 target/linux-board-regression-0.4.6.json，均绑定实际 release 的 SHA-256。临时文件位于唯一 /tmp 目录，系统工具权限不变。

## Shell 环境与辅助命令验收

用户启动 0.4.6 后复现两项环境差异：父 Shell 的 printf 函数会接管短命令的完成标记；用户 PATH 不含 sh 时，长命令无法启动辅助 Shell。两者都没有误报成功，但会使已执行命令退化为超时。0.4.7 使用 `command printf` 输出内部标记，以 `command -p sh` 查找辅助 Shell，不改变用户命令看到的 PATH。

新增 13 项环境检查，qualification 在 Windows 上共 87 项：printf 函数/别名下长短命令准确完成，文件传输的标记同样不受函数影响；PATH 被设为 /dev/null 时长短命令仍可执行且看到同一 PATH；带中文、引号及空格的导出变量完整继承；子命令重定向 stdout、set -e/set -u 或非零退出都保留准确完成状态。每个临时环境修改均在 finally 恢复。

资格验收及 30 项常规回归报告分别为 target/linux-qualification-0.4.7.json 和 target/linux-board-regression-0.4.7.json，绑定实际 release SHA-256。系统 lrz/lsz 权限保持 0755，测试只写唯一临时目录。

## eval 别名与实际命令执行验收

用户启动 0.4.7 后，通过当前 MCP 在登录 Shell 临时设置 `alias eval='echo eval-shadowed'`。短 exec 返回 completed=true、exit_code=0，但输出为别名打印的命令文本，原请求没有执行；长命令在新 Shell 中执行，不受该别名影响。原始复现记录为 target/eval-alias-0.4.7-reproduction.json。测试后立即恢复别名。0.4.8 内部改用 `command eval`，绕过同名别名及函数，不更改用户命令本身。

新增 6 项实机检查，Windows qualification 共 93 项：eval 别名存在时，长短 exec 均产生实际输出与预期非零退出码，长短 Shell 同步的 ZMODEM 下载内容完整一致；另验证长短语法错误均返回非零退出及完成状态，下一条命令可以正常执行。可选 POSIX 测试先断言测试 Shell 已启用别名展开，再验证 exec 和文件命令两种包装的输出与退出码；修复前该用例失败，修复后通过。

资格验收与常规回归报告分别为 target/linux-qualification-0.4.8.json（93 项）和 target/linux-board-regression-0.4.8.json（30 项），均记录实测 release SHA-256。测试使用系统 lrz/lsz，不改变原文件权限，临时修改的别名、PATH 与工作目录均已恢复，唯一 /tmp 测试目录已清理。

同一 release 另通过 21 项恢复检查，报告为 target/linux-recovery-0.4.8.json：最长 64000 字符输入与 3000 字节 UTF-8 引用内容完整；四协议缺失文件后 Shell 继续可用；真实中断留下 10240 字节部分文件后正确退出；四协议手动握手的 1025 字节载荷往返一致，XMODEM 额外尾部仅为 0x1A。三组共 144 项实机检查，报告均绑定同一二进制 SHA-256，临时文件全部清理。

## 接收上限、取消与 X/YMODEM 额外应答

用户启动 0.4.8 后，扩展的 108 项 qualification 全部通过，报告为 target/linux-qualification-session-0.4.8.json。新增覆盖接收上限后的完整续读与迟到完成标记隔离、全部 256 种字节值的小块读取与模式切换、取消读取后继续接收延迟输出，以及 compact/detailed 下三种表示在较小 output_limit 下的分页。每页只推进实际展示的 UTF-8 字节，output_id 保持不变。

随后常规回归发生一次 XMODEM 上传重试耗尽，远端 lrz 仍在等待，使用协议 CAN 序列后恢复并清理临时目录。原始异常没有完整线上抓包；另用接收程序启动前输出 40 个 CRC 请求字符，稳定复现同样的重试上限错误，记录为 target/packet-noise-0.4.8-reproduction.json。旧实现对每个无关字符都立即重发整个数据包，可能连续发送重复包并迅速耗尽重试次数。

0.4.9 的数据包发送等待实际 ACK、NAK、连续 CAN 或超时，无关应答不会触发重发，累计达到 4096 个无关字节后取消。三项内存协议测试验证额外 CRC 请求不重发、噪声有界取消、对端 CAN 取消；旧实现未通过前两项。原有 NAK 的当前数据包重传保持测试覆盖。实机 qualification 新增 XMODEM、XMODEM-1K 和 YMODEM 的额外 CRC 请求上传、下载逐字节校验与后续 Shell 操作，共 117 项。

修正版 117 项 qualification 与 30 项常规回归全部通过，报告为 target/linux-qualification-0.4.9.json 和 target/linux-board-regression-0.4.9.json。噪声场景的 YMODEM 下载完整 1320 字节与源文件逐字节一致；XMODEM 系列额外尾部仅为 0x1A。报告记录同一实测 release 的 SHA-256。

同一 release 的 21 项恢复检查也全部通过，报告为 target/linux-recovery-0.4.9.json，包含最长 64000 字符命令、真实传输中断后的 10240 字节部分文件检查，以及四种协议的手动握手往返。三组共 168 项实机检查，临时文件已清理，PATH/cwd 和系统工具权限保持测试前的值。

## 0.4.9 新会话与文件长度矩阵

用户重新启动 0.4.9 后，当前构建的 117 项 qualification 再次全部通过，报告为 target/linux-qualification-session-0.4.9.json。新增 test_linux_matrix.py 对四协议各十种文件长度做往返，覆盖空文件、128/1024 字节块边界和 XMODEM 序号回绕；非空载荷末尾含真实 0x1A，校验不能将它当作填充删除。

初版矩阵脚本没有为 ZMODEM 接收显式选择目录，空文件实际写入登录目录，随后从 /tmp 下载时报告不存在。这是测试脚本的目录设置错误，未计为服务缺陷；已清理误放文件。修正后 Y/ZMODEM 在自定义 Shell 接收命令中进入唯一 /tmp 目录，并用文件名作为元数据，XMODEM 保留显式绝对目标路径。

修正后的矩阵全部 40 组往返、123 项检查通过，报告为 target/linux-matrix-verified-session-0.4.9.json。当前构建另通过 30 项常规回归和 21 项恢复检查，报告为 target/linux-board-session-0.4.9.json 和 target/linux-recovery-session-0.4.9.json。加上 117 项 qualification，本轮共 291 项实机检查，四份报告绑定同一当前运行程序的 SHA-256。临时目录均已清理，未修改服务实现和版本号。
