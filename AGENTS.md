# 编码代理约定

优先使用简体中文沟通。项目是 Windows x64 UART MCP 服务，同时支持终端和原始字节设备。优先保证目标明确、执行状态可信、数据不静默丢失、反馈精简可追索。避免无关依赖和为拆分而拆分的重构。

## 阅读入口

先读 [README](README.md) 理解用户安装与使用，再按任务选择：

| 任务 | 文档 |
| --- | --- |
| 使用串口工具、核对对外行为 | [Agent 指南](docs/agent-guide.md)、[工具手册](docs/tools.md) |
| 定位模块、修改服务实现 | [开发指南](docs/development.md)、[实现与协议](docs/architecture.md) |
| 选择验证方法、复现实机问题 | [测试与验收](docs/testing.md)、[Linux 实机验收](docs/testing-linux.md) |
| 修改默认值、响应或兼容行为 | [升级说明](docs/migration.md) |

根目录存在 `.codegraph/` 时，理解或定位代码前优先使用 CodeGraph；否则使用 `rg`，不要自行索引。

## 必须保持的行为

- 同端口操作独占，冲突立即 busy，不排队写入；注册表锁内不得阻塞 I/O，不同端口可独立工作。
- 写入或文件传输失败不重试整个操作，部分写入的结果不确定；协议规定的当前数据包重传可以保留。
- terminal/binary 是交互配置，不是系统识别；binary 默认 hex，不注入隐式 Shell。
- 匹配后的字节留给后续读取和手动文件协议；只有显式清输入或连接重置才丢弃。
- 提示符和静默不能证明 Shell 完成；exec 只有解析到专属标记和退出码才报告 completed。内部求值与标记绕过同名函数/别名，辅助 Shell 查找不依赖用户 PATH，且不能修改命令看到的 PATH。
- exec 长短命令默认 stdin 均为 EOF；文件启动命令必须保留原始串口 stdin。取消本地读取不等于停止远端程序。
- compact 只输出一份结果，展示量与接收量分离；截断可见，分页不得跳过未展示数据。
- 参数错误为 JSON-RPC -32602；设备/文件失败为 isError=true；错误使用英文；structuredContent 必须是对象。
- 诊断写 stderr，默认回环、拒绝浏览器 Origin，不隐藏无认证和客户端共享连接的事实。

## 修改与验证

修改工具时同步检查分发、参数校验、`src/mcp/schema.rs`、行为测试及用户手册；默认值和响应结构变化写入升级说明。测试应覆盖外部可观察行为及真实失败模式，避免仅复述实现。

Rust 代码变更完成后执行：

```powershell
cargo fmt --all -- --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --release
```

可选 POSIX Shell 测试、运行中 exe 的独立构建方法见开发和测试文档。默认测试不得触碰真实端口；只有用户明确提供测试设备后才运行对应硬件脚本。回环不能证明真实 Shell 或对端文件传输完成。

文档变更校验本地链接、JSON 工具示例与当前 Schema；新增或迁移文档后，检查发布包内文档及链接是否齐全。README 保留安装和操作入口，当前工具行为放工具手册，技术实现放架构文档，测试方法与验收结果分开记录，历史过程归档到 `docs/history/`。
