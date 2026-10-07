<!--
flattencom - Chinese Readme Guide

Introduces the project components, startup commands and documentation links.

Authors:
worryzu <worryzu@gmail.com> @LinearTeam

Copyright (C) 2026 Evarentha
SPDX-License-Identifier: GPL-3.0-or-later
-->

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/images/logo-dark.png">
    <source media="(prefers-color-scheme: light)" srcset="docs/images/logo-light.png">
    <img src="docs/images/logo-light.png" alt="flattencom" width="360">
  </picture>
</p>

<h1 align="center">flattencom</h1>

<p align="center"><a href="README.md">English</a> | <b>简体中文</b></p>

<p align="center">
  面向 <b>Linux 和 Windows</b> 的串口调试工作台：Qt 桌面、CLI/TUI 与 MCP 桥接共用同一本地后台服务。
</p>

## 开始使用

完整解压安装包，启动 `bin/flattencom-gui`（Windows：`bin\flattencom-gui.exe`）。Windows 需保留全部 DLL 和 `platforms` 目录。未部署 Qt 的 Linux TGZ 需要系统 Qt。选择“文件 > 打开串口”，确认串口参数及录制目录后连接。GUI 新会话默认录制可读 `.log` 文件。

在 Linux 上从源码构建时，安装 Rust 1.98+、CMake 3.24+、Qt 6.5+ Widgets/Network/Test、Ninja、C++20 编译器、libudev 开发文件和 pkg-config。Debian/Ubuntu 使用单独安装的 Qt SDK 时，还需安装 `libgl1-mesa-dev` 和 `libxkbcommon-dev`。项目脚本需要 Python 3.9+。在仓库根目录执行：

```bash
cargo build --release --workspace --locked
cmake -S gui -B gui/build/release -G Ninja -DCMAKE_BUILD_TYPE=Release
cmake --build gui/build/release --parallel
./gui/build/release/flattencom-gui
```

Windows 本机构建见 [MSVC + Qt 步骤](docs/DEVELOPMENT.md#windows-build)。只有 GUI 需要 Qt；Cargo 命令构建 CLI、后台服务及 MCP 桥接程序。打包脚本将本地产物写入 `dist/`。

无需硬件，用虚拟回环检查直连 IO：

```bash
./target/release/flattencom send virtual://echo AT --expect AT
./target/release/flattencom send virtual://echo --hex "00 FF 0D 0A" --newline none
```

Echo 只回送字节，不执行命令。Windows 使用 `target\release\flattencom.exe`。

## 共享与直连

```text
GUI / CLI attach / MCP bridge --> flattencomd --> serial device
MCP client -- stdio -----------> MCP bridge
CLI monitor / send / receive ------------------> serial device
```

- GUI/MCP 可按需启动后台。打开已连接/重连中的共享端口会加入原会话，不改变参数和录制。
- 关闭 GUI 或仅关闭视图后，后台采集仍会继续。“会话 > 关闭共享串口会话”会释放端口；`flattencom daemon stop` 关闭后台全部会话。
- CLI monitor/send/receive/autobaud/replay 直接打开设备。后台已持有端口时使用 attach/rpc：

```bash
./target/release/flattencom sessions
./target/release/flattencom attach SESSION_ID
```

将 SESSION_ID 替换为返回的标识。共享参数/写入影响所有客户端，本地暂停/显示不影响他人。见 [GUI](docs/GUI.md) 和 [RPC 示例](docs/PROTOCOL.md#reproducible-local-example)。

## 能力

- 发送文本、HEX 或文件；连接期间修改串口参数、控制线及 BREAK。
- 浏览连续 RX 文本、RX/TX 数据块和独立发送历史；搜索、高亮、标记和测量所选输出的时间间隔。
- 按设备保存配置，插入宏，周期发送，并配置适合具体板卡的复位步骤。
- 录制可读日志、JSONL 数据帧或原始 RX；浏览磁盘历史，导出 HEX/CSV/JSONL/PCAP，回放 JSONL。
- 解码文本、HEX、JSON 行、Modbus RTU 和 NMEA；添加进程或 WASM 解码器及匹配触发动作。
- 使用 MCP 工具、资源、五个提示词模板、固定选区和可取消作业。

可读录制和导出默认用 **.log**，问题报告用 **.txt**。导出包含内存中保留的数据帧；更早的历史需读取已保存的录制文件，回放需使用 JSONL。格式和保留规则见[录制与导出](docs/RECORDING.md)。

## MCP 与界面偏好

对于支持 `mcpServers` 的客户端，将示例中的程序路径替换为真实绝对路径：

```json
{"mcpServers":{"flattencom":{"command":"/absolute/path/to/flattencom-mcp","args":["--lang","zh-CN"]}}}
```

Windows 用 `.exe`。“帮助 > 接入 MCP”显示本机 MCP 配置示例。先 list_ports/list_sessions，再 open_port、send、read_frames、read_sent。运行中的 MCP 桥接程序可恢复与后台服务的连接；结果未知的写操作不会自动重发。共享选区是固定文本快照。若要将读取权限限定为指定选区，使用 `flattencom-mcp --selection SELECTION_ID`。详见 [MCP](docs/MCP.md)。

默认使用英文；可通过“视图 > Language”切换界面语言，无需重启。`--lang zh-CN` 和 FLATTENCOM_LANG 为支持的客户端选择中文。“视图 > 主题”提供跟随系统、浅色、深色。设备字节和协议标识不翻译。环境设置及优先级见[国际化](docs/I18N.md)和[排障](docs/TROUBLESHOOTING.md)。

## 平台与检查

| 平台 | 支持范围 |
|---|---|
| Linux x64 | GUI、CLI/TUI、后台服务和 MCP |
| Windows x64 | GUI、CLI/TUI、后台服务和 MCP |
| macOS | 不支持 |

Linux 和 Windows 支持 None/Odd/Even/Mark/Space 校验及 1/1.5/2 停止位，具体能力取决于驱动和硬件。1.5 停止位要求 5 数据位；2 停止位要求 6–8 数据位。波特率/启动检测和 Modbus 分帧存在启发式限制。见[行为与限制](docs/DEVELOPMENT.md#behavior-and-limits)。

```bash
python3 scripts/check.py
python3 scripts/check-docs.py --smoke
```

完整检查包含格式/lint、Rust/Qt 测试和隔离 GUI 回环；文档 smoke 需要 release 程序。打包命令见[开发](docs/DEVELOPMENT.md#package-and-install)。

## 组件与文档

| 组件 | 职责 |
|---|---|
| `flattencom-gui` | Qt/C++ 桌面客户端 |
| `flattencom` | CLI/TUI，直连或共享 |
| `flattencom-mcp` | stdio MCP 桥接 |
| `flattencomd` | 共享会话、录制、作业 |
| `flattencom-core` | 同步 Rust 传输/会话/解码/录制库 |
| `flattencom-proto` | RPC 类型、消息分帧及 Rust 客户端库 |

Qt 经 Unix socket/Named Pipe 传 JSON-RPC，无 Rust FFI；GUI 历史直接读本地文件。数据流和源码阅读顺序见[架构](docs/ARCHITECTURE.md)。

[GUI](docs/GUI.md) · [MCP](docs/MCP.md) · [录制](docs/RECORDING.md) · [协议](docs/PROTOCOL.md) · [工作台 RPC](docs/WORKBENCH-RPC.md) · [开发](docs/DEVELOPMENT.md) · [国际化](docs/I18N.md) · [文案](docs/UI-TEXT.md) · [路线图](docs/ROADMAP.md)

## 许可证

本项目以 GPL-3.0-or-later 许可发布，Copyright (C) 2026 Evarentha，完整文本见 [LICENSE](LICENSE)。
