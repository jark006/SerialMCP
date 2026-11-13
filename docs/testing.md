# 测试与验收

本文说明如何验证改动。已通过的设备和报告见 [Linux 实机验收](testing-linux.md)，源码构建见 [开发指南](development.md)。普通安装和日常操作不需要执行本页的测试脚本。

## 默认自动测试

在源码仓库根执行：

```powershell
cargo fmt --all -- --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --release
```

默认 Rust 测试不访问真实串口，覆盖 JSON-RPC/HTTP、工具 Schema 与参数校验、端口独占、读取分帧和取消、Shell 标记、输出分页及内存 X/Y/ZMODEM 会话。

## 可选 POSIX Shell 测试

这些测试默认 ignored，需要可用的 POSIX Shell。Windows 安装 Git Bash 后，可在同一 PowerShell 会话执行：

```powershell
$env:SERIAL_MCP_TEST_SHELL = 'C:\Program Files\Git\bin\bash.exe'
cargo test --locked -- --ignored
```

覆盖引号、多行和长命令、默认 EOF 与原始终端输入、printf 函数、eval 别名，以及 PATH 不含 sh 时的辅助 Shell 启动。它们验证包装的实际执行行为，不替代开发板上的串口和文件协议验收。

## 硬件测试前提

只有用户明确提供并授权使用测试设备后，才运行对应脚本。Python 脚本仅依赖 Python 3 标准库，使用 `--port` 指定端口，`--exe` 指定被测程序，`--report` 指定报告路径。以下 COM3 只是示例，应按实际设备选择。

脚本会启动自己的临时服务。运行前关闭其他服务对该 COM 口的连接，保持旧服务进程本身运行也可以；不同服务无法同时占用同一个串口。程序退出后临时服务释放端口，再由正常 MCP 服务重新打开。

被测 exe 正在运行且需要重新构建时，使用独立 `target/verified` 目录，方法见开发指南。文件传输失败后先检查对端状态，不直接重跑整套测试或整个文件。

## TX/RX 回环

**仅在 TX/RX 已短接的专用回环端口运行**，不能对 Linux 控制台、Bootloader 或其他已连接设备运行：

```powershell
python scripts/test_loopback.py --port COM3 --exe target/release/serial-mcp.exe --report target/loopback-report.json
```

脚本发送全部字节值和控制字符，覆盖二进制往返、分帧尾部、编码、按键、分页、busy、状态、取消、重连和关闭；还检查回显不能冒充 Shell 完成。回环不能证明真实 Shell、对端文件传输或物理断电恢复正常。

## Linux 开发板

先确认目标已登录 POSIX Shell，不要在登录提示符或 Bootloader 上运行。目标需要常用 BusyBox/POSIX 工具，以及可执行的 lrz/lsz；恢复测试还需要 timeout。脚本使用唯一 `/tmp` 目录，结束时清理；不改变系统 lrz/lsz 的权限。临时环境修改会恢复，发生失败时应核对报告和清理结果。

| 脚本 | 当前检查数 | 覆盖 |
| --- | ---: | --- |
| `scripts/test_linux_board.py` | 30 | Shell 完成、退出码、超时/取消、并发响应、四协议往返和重连 |
| `scripts/test_linux_recovery.py` | 21 | 长输入、缺失文件、真实传输中断、手动握手和后续 Shell 恢复 |
| `scripts/test_linux_qualification.py` | 117（Windows） | 输入语义、Shell 环境、额外 CRC 请求、多文件拒绝、落盘竞态、原始字节和分页 |
| `scripts/test_linux_matrix.py` | 123 | 四协议各十种长度，共 40 组双向传输及下一命令可用性 |

依次执行所需脚本，每项通过后再继续；失败时先保留现场和错误信息：

```powershell
python scripts/test_linux_board.py --port COM3 --exe target/release/serial-mcp.exe --use-system-tools --report target/linux-board-report.json
```

```powershell
python scripts/test_linux_recovery.py --port COM3 --exe target/release/serial-mcp.exe --report target/linux-recovery-report.json
```

```powershell
python scripts/test_linux_qualification.py --port COM3 --exe target/release/serial-mcp.exe --report target/linux-qualification-report.json
```

```powershell
python scripts/test_linux_matrix.py --port COM3 --exe target/release/serial-mcp.exe --report target/linux-matrix-report.json
```

board 脚本默认从 `/system/bin` 复制 lrz/lsz 到临时目录并给副本执行权限；上面的 `--use-system-tools` 改为直接使用已有可执行原程序。其他来源可用 `--modem-source` 指定目录。不要为了通过测试改动系统分区原文件的权限。

qualification 的环境测试要求起始 printf/eval 为默认 builtin，避免覆盖用户自定义函数或别名。字节流测试按已启用 ONLCR 的目标终端比较 LF 展开后的字节，不修改终端设置。Windows 文件名失败用例不能直接套用到其他系统。

matrix 验证 0、1、127、128、129、1023、1024、1025、32769、65537 字节。非空载荷末尾含真实 `0x1A`；XMODEM 只允许原始长度之后的协议填充，Y/ZMODEM 必须等长。Y/ZMODEM 的接收命令显式进入测试目录，不能只依赖 remote_path 的目录部分。

recovery 根据 `timeout --help` 选择 BusyBox 参数形式，并断言确实留下小于源文件的部分文件，不能把程序启动失败当成传输中断通过。

## 报告与收尾

以报告中的版本、exe 路径、SHA-256、通过项和 cleanup 判断测试对象与结果，不按文件名推断。matrix 还记录每组源文件/接收文件的长度、SHA-256 和耗时，失败时记录具体错误。

测试结束后检查临时目录、PATH/cwd、工具权限和远端程序状态，再恢复正常 MCP 的串口连接。取消读取不等于停止远端进程，传输失败也不保证对端已经退出。

`target/` 报告是验收机本地产物，不随源码或发布包分发。记录新结论时，在实机验收页给出条件、覆盖和报告索引；详细过程放入 [历史记录](history/linux-qualification.md)，避免将历史检查数混作当前脚本的覆盖范围。
