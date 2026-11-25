# Remove Zed AI, Accounts, and First-Party Services Implementation Plan

> **For agentic workers:** REQUIRED SKILL: Use implement to execute this plan task-by-task. Respect the handed-off git_state: inherit prepared, otherwise prepare; use git-workflow for checkpoint/finalize. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal**

交付可独立构建的 Zed 编辑器，保留已批准的 Git、本地开发、扩展和远程开发能力。

**Scope Source**

[已批准的需求](../../scope/2026-11-25-remove-zed-account-and-services.md)

**Architecture**

先从保留组件拆除 AI 调用方，再裁剪账号、协作和官方服务。以 Cargo 依赖、启动路径和保留功能的验证确定共享代码的去留。

**Tech Stack**

Rust、Cargo、GPUI、Windows x64、Inno Setup 和 Scoop。

**Verification**

每项先找出功能入口和保留功能的调用方。每批构建 GUI 和 CLI，核对动作、设置与依赖。最终按需求执行隔离的 Windows、Git、扩展、SSH、网络和安装验收。

## 执行顺序

- [x] 移除 Git 中的 AI 提交信息生成、分支差异审查和冲突处理入口及设置。核对手动冲突操作、GitHub PR 链接、普通 Git 动作和凭证调用链。
- [x] 移除 ACP 日志和 Agent 内置网页搜索。清理 `crates/zed` 的初始化与工具栏项、`crates/agent` 的工具注册，以及 `acp_tools`、`web_search`、`web_search_providers` 的成员定义。核对普通 LSP、调试日志和工作区文本搜索的入口。
- [x] 移除内置 GitHub Copilot 模型、编辑预测提供方和 OAuth 界面。旧的 `edit_predictions.provider` 中的 `copilot` 解析为 `none`。核对 GitHub 提供方依赖和应用动作清单。
- [x] 移除编辑预测的提供方、请求和配置界面。裁剪 `zed` 的预测初始化和状态栏入口，以及 `settings_ui`、默认键位、专属 crate 和文档中的配置路径。验证 Windows 构建、预测提供方的依赖图，以及普通 LSP 补全和 Git 动作。
- [x] 移除 `editor` 和 Vim 中的编辑预测动作、渲染与内部状态。保留普通 LSP 补全、代码操作和片段导航。
- [x] 移除 Agent 面板、内联助手、外部 Agent 与 ACP、模型、MCP、提示词和 Skills。拆除 `zed`、`workspace`、`editor`、`project`、`extension` 与设置中的入口和依赖。构建 Windows GUI 和 CLI，核对编辑器与 Git。
- [x] 移除 `crates/client`、`crates/title_bar`、`crates/zed`、`crates/workspace` 和 `crates/project` 的 Zed 登录与协作，保留 SSH 共用的传输和协议。检查旧配置的兼容性。
- [x] 裁剪扩展市场、官方远端下载、遥测、崩溃采集和自动更新。核对 `crates/remote`、扩展宿主、构建脚本和安装器中的保留路径及依赖。
- [ ] 执行 Windows x64、Git、扩展、SSH、网络和安装验收。核对变更范围和工作区状态。

## Git 验证基线

- Windows x64 `release-fast` 的 GUI 和 CLI 构建通过。对比 GUI 的 `--dump-all-actions`，保留了 77 个 Git 动作，包括 `git::Clone`、`git::Commit`、`git::StageAll`、`git::Fetch`、`git::Pull`、`git::Push` 和 `git::CreatePullRequest`。指定 AI 动作均已移除。
- GUI 使用包含旧 AI 设置和键位的隔离配置启动，应用日志确认打开测试 Git 仓库。沙箱限制桌面交互，Git 面板交互仍需验证。
- `git_ui` 的 209 项现有测试全部通过，覆盖手写提交、暂存、分支、差异、历史和冲突处理。
- 隔离本地 Git CLI 验证已通过。覆盖 clone、stage、手写 commit、push、fetch 和 pull。另用启用 TLS 与 Basic 认证挑战的本地 Git 远端，验证了凭证助手认证后的 clone、push、fetch 和 pull。Zed 内的同类操作和私有 GitHub 认证仍需验证。
- `crates/git/src/repository.rs` 的 Git 命令和 AskPass、`crates/git_hosting_providers/src/providers/github.rs` 的链接构造、`crates/project/src/git_store.rs` 的 Git 仓库代理继续供本地和远程项目使用。`cargo test --package git_hosting_providers --lib providers::github::tests` 已通过 22 项测试，覆盖 GitHub HTTPS、SSH 地址解析及 PR 链接生成。
- 清理账号时，保留 `git_hosting_providers::init`、GPUI 的普通 HTTP 客户端，以及 SSH 远程项目使用的 `ProjectId` 和 Git 消息。移除协作生成共同作者的入口，保留 Git 提交信息里手写的 `Co-authored-by`。

## ACP 日志和 Agent 网页搜索验证

- Windows x64 GUI 和 CLI 使用 `--offline --locked --profile release-fast` 构建成功。`cargo tree` 确认三个已移除 crate 均不可达，Git、凭证助手、编辑器、LSP、调试和文本搜索仍在依赖图中。
- `--dump-all-actions` 确认 `dev::OpenAcpLogs` 已移除，其余动作保持上一阶段的清单。最新产物使用隔离配置渲染了首帧并打开测试 Git 仓库。

## 内置 Copilot 验证

- Windows x64 GUI 和 CLI 构建成功。`cargo tree --offline --locked` 确认三个 Copilot crate 均不可达。新版 GUI 动作清单移除了 `copilot_edit_predictions::Reinstall`，仍保留 77 个 Git 动作。
- `settings_content` 的 `test_legacy_copilot_provider_loads_as_disabled` 通过。新版 GUI 使用含旧 Copilot 设置的隔离配置渲染了首帧并打开测试 Git 仓库。
- Copilot CLI 的推荐入口已移除。通用外部 Agent 注册表随剩余的外部 Agent 功能清理。

## 编辑预测和 Agent 入口验证

- 移除编辑预测后的 Windows x64 GUI 和 CLI 构建通过。隔离配置包含旧 Copilot 提供方和预测键位，应用渲染首帧并打开测试 Git 仓库。动作清单移除了 22 个预测动作，仍保留 77 个 Git 动作和普通补全的显示及确认动作。
- 移除 Agent 界面和启动入口的 Windows x64 GUI 和 CLI 中间版本构建通过。隔离配置启动后渲染首帧并打开 Git 仓库，启动日志没有 Agent、模型或提示词初始化记录。
- 项目组状态仍由 Workspace 维护。项目可从标题栏和 Open Recent 的 This Window 分组切换。SSH 协议中保留已移除消息的编号，扩展宿主继续解析旧的能力声明并跳过 MCP 加载。
- AI 清理累计删除 50 个 crate 和 668 个文件。Git 差异含约 51 万行删除，其中包括测试、样本、资源和文档。锁定的 Windows 构建依赖图中，GUI、CLI 和远端服务均已移除这些 crate。Git、凭证助手、普通 LSP、终端、调试和扩展仍在依赖图中。
- 普通编辑器补全的 25 项测试全部通过。Git 托管提供方的 139 项测试全部通过，包含 GitHub 的 22 项。设置测试通过 32 项，1 项按原配置跳过。20 份默认及替代键位文件通过解析检查。
- 最终 Windows x64 `release-fast` 的 GUI、CLI 和远端服务使用 `--offline --locked` 构建通过。最终 GUI 相比基线移除了 147 个 AI 相关动作，保留 77 个 Git 动作，以及普通编辑器补全、LSP、终端、项目、扩展和调试入口。CLI 和远端服务的版本命令正常退出。
- 最终 GUI 使用含旧 Copilot、模型、MCP、Agent、Anthropic 选项和键位的隔离目录启动。程序渲染首帧并打开本地 Git 仓库。沙箱限制 GUI 鼠标交互。私有 GitHub 仓库认证和 SSH 会话仍列入最终验收。

## 账号、协作和官方服务验证

- 移除客户端账号凭证、认证连接、协作面板、协作后端与通话组件。保留本地 HTTP 代理、Git 凭证交互和远端项目独立使用的 RPC 类型。
- 扩展页面沿用本地安装、卸载、主题和调试能力。SSH、WSL 和开发容器使用已部署的远端服务，连接失败时提示位置和版本。Windows 打包脚本去除更新助手的构建、签名和暂存项。
- 裁剪遥测事件和远端转发、崩溃采集和上传。保留日志、工作区诊断和本地系统信息复制。锁定的 Windows 依赖图已核对，保留 Git、AskPass、编辑器、LSP、调试、终端、扩展宿主和远端服务。
- Windows x64 GUI、CLI 和远端服务通过离线锁定构建。当前 GUI 相比去 AI 版移除 51 个动作，保留全部 77 个普通 Git 动作以及应用菜单、LSP、项目、终端和调试动作。20 份内置键位未引用已移除动作。远端服务的 `version --package` 返回 `1.29.3+dev`。
- 最终 GUI 使用旧的 AI、服务器 URL、凭证 URL、遥测、自动更新和扩展自动安装配置启动。日志确认渲染首帧并打开本地 Git 仓库，本地主题扩展被加入扩展和主题索引。
- 本地 Git HTTPS 测试处理 6 次认证挑战、10 次已认证请求，使用凭证助手完成 clone、手写 commit、push、fetch 和 pull。Git 凭证助手、Git 仓库命令及 Git 托管提供方源码通过变更范围核对。
- 使用 Windows x64 `release-fast` 离线锁定测试通过 223 项。Git 托管提供方 139 项、远端开发 52 项、设置 32 项，另有 1 项设置测试按原配置跳过。远端服务同版本、缺失和版本不符的校验均已覆盖。
- 同为 Windows x64 `release-fast` 的 `zed.exe` 从去 AI 版的 300.3 MiB 降至 261.1 MiB，减少约 13.1%。
- 更新与安装脚本通过 PowerShell 语法检查。Scoop 替换通过预演。46 份删除文档的链接检查通过。当前机器没有 Inno Setup、WSL 发行版或 Docker，安装包和真实 WSL、容器会话待相应环境验收。正式 SSH 会话也待实机验收。
