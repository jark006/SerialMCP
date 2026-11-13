# 开发指南

本项目使用 Rust 2024、Axum/Tokio、Serde、serialport 和 zmodem2，发布目标为 Windows x64。用户安装见 [README](../README.md)，模块职责与实现约束见 [实现与协议](architecture.md)，测试方法见 [测试与验收](testing.md)。维护仓库时遵循根目录 `AGENTS.md`。

## 环境与构建

安装 Rust stable 和 MSVC C++ 构建工具，在仓库根执行：

```powershell
cargo fmt --all -- --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --release
```

默认产物为 `target\release\serial-mcp.exe`。`.cargo/config.toml` 为 Windows x64 MSVC 启用静态 CRT；release 使用 LTO、单 codegen unit 和体积优化，发布程序无需另装 VC 运行库。

## 与正在运行的版本并行构建

Windows 不能直接覆盖正在运行的 exe。需要保留现有服务时，将新版本构建到独立目录：

```powershell
cargo build --locked --release --target-dir target/verified
```

新文件为 `target\verified\release\serial-mcp.exe`。现有服务不会自动切换到新程序；需要升级时退出旧服务、启动新程序，并重新连接 MCP。独立测试服务还需要不同的监听端口，访问同一串口前必须释放原连接。

## 修改工具或行为

1. 对照 `src/mcp.rs` 的分发与校验、`src/mcp/schema.rs` 的工具定义，保持参数、默认值和实现一致。
2. 修改对应模块，保留端口独占、字节尾部、执行完成判定和错误分类等约束。
3. 用外部可观察行为或真实故障模式验证；默认 Rust 测试不得访问硬件，Shell 和实机测试按测试文档选择。
4. 当前用法更新 [Agent 指南](agent-guide.md) 和 [工具手册](tools.md)。默认值、结果结构及兼容行为的变化写入 [升级说明](migration.md)。

## 打包

版本来自程序的 `--version` 输出，与 Cargo 包版本一致。默认打包 release 程序：

```powershell
.\scripts\package.ps1
```

指定独立构建的二进制：

```powershell
.\scripts\package.ps1 -BinaryPath target/verified/release/serial-mcp.exe
```

产物为 `target/dist/serial-mcp-<版本>-windows-x64.zip` 和同名 `.sha256`。包内包含程序、README、LICENSE 和完整 `docs/`，保持相对链接可用。源码、测试脚本及本地 `target/` 验收报告不随包分发；开发和复测命令在源码仓库中执行。

可用 `-OutputDirectory target/package-check` 将试打包放到独立目录，避免覆盖已有发布产物。打包前核对二进制版本和来源；实测过的构建应与报告中的 SHA-256 一致。

Windows CI 执行 fmt、test、clippy、release 构建和打包，并上传构建产物。发布到 GitHub Releases 是独立的维护操作。

## 文档维护

| 内容 | 放置位置 |
| --- | --- |
| 安装、接入和日常使用入口 | `README.md` |
| Agent 操作流程和调用示例 | `docs/agent-guide.md` |
| 当前参数、默认值、结果与故障处理 | `docs/tools.md` |
| 并发、缓冲、Shell 包装和协议内部行为 | `docs/architecture.md` |
| 构建、修改和发布流程 | 本文 |
| 测试前提、命令和覆盖范围 | `docs/testing.md` |
| 当前实测结论和报告索引 | `docs/testing-linux.md` |
| 升级兼容变化、历史过程 | `docs/migration.md`、`docs/history/` |

只修改文档时校验链接、JSON 示例、当前工具 Schema 和打包内容；不因整理文档触碰真实串口。新增文档由打包脚本随 `docs/` 一起复制，不再维护单独的文档文件清单。
