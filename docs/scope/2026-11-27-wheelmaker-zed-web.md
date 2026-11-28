> 由 scope skill 于 2026-11-27 生成
> 状态：已批准 2026-11-28
> 读者：审阅者与实施人员

# WheelMaker 接入 Zed Web

## 目标

用户在任意支持 WebGPU 的电脑上访问 WheelMaker 的 Zed HTTPS 入口，用 WheelMaker 账号登录并打开有权限的项目。编辑器沿用本地这份 Zed 源码，由项目所在的 Windows Hub 提供文件和语言服务。Zed Web 可以独立于 WheelMaker 发布和更新。

## 决策基线

### 需求决策

- 首版面向启用 Zed Web 的 Windows x64 Hub。项目可以分布在不同 Hub。用户通过 WheelMaker 已有的项目目录选择项目，并在同一浏览器标签页切换。
- 登录后进入 Zed 编辑器。标题栏右侧显示 WheelMaker 连接信息和项目切换入口。这套入口由 Zed Web 提供，首版交付范围为项目浏览、文件编辑和语言服务。
- Web 使用目标 Hub 的全局 Zed 配置和项目配置，呈现对应的当前主题、字体及可用快捷键。保留现有 Web 原型的中文显示和 TSX 高亮，代表性项目冷启动下载量以 5 MB 以内为目标。
- 项目文件可编辑保存。语言服务应提供补全、悬浮提示、内联提示和跳转定义，已识别的 SDK、标准库与依赖源码通过定义跳转以只读方式打开。

### 技术决策

- 复用 Registry 登录、浏览器会话和项目权限。以 `registry.zed.publicUrl` 指定 Zed HTTPS 入口，例如 `https://zed.wheelmaker.top`。Gateway 使用该地址挂载受管站点，Registry 使用同一地址校验登录回跳。
- Zed Web 静态包和 Windows x64 `remote-server.exe` 从同一次构建发布，共用 Zed 组件版本及其清单。构建工作流放在 Zed 仓库并使用 GitHub 托管 Runner。WheelMaker 发布页使用独立的 Zed Web 卡片和发布按钮，由发布器调度构建、取回产物并提交到 Release Server。Gateway 启动时通过代码中固定的发布清单地址检查更新，目标 Hub 由 WheelMaker 安装配套的可执行文件。
- 浏览器通过 Zed 站点的同源 WebSocket 连接 Gateway，Gateway 和 Registry 复用 WheelMaker 到目标 Hub 的现有路由及共享隧道。Hub 启动与页面版本匹配的 `remote-server`，沿用 Zed 编辑协议传输项目数据和语言服务结果。

## 设计视图

### 功能设计

访问 Zed 入口时，已有有效 WheelMaker 浏览器会话的用户直接进入编辑器。需要登录时进入 WheelMaker 登录流程，完成后回到配置的 Zed 地址。编辑器内选择器显示账号有权访问的已登记项目和连接状态。打开项目时显示连接进度，Hub 离线或版本准备失败时给出对应状态。

切换项目时先走 Zed 的未保存内容处理流程。用户取消则留在当前项目。目标项目连接并初始化成功后再替换当前工作区，目标连接失败时保留原项目及其连接。标题栏在切换成功后更新项目和 Hub 信息。

### 技术设计

#### 组件职责

| 组件 | 职责 |
| --- | --- |
| Zed Web | 运行现有 WebGPU 编辑器。提供登录入口、项目选择器和同标签页工作区切换。使用版本化的编辑器资源及当前项目需要的资源。 |
| Zed 构建工作流 | 从同一提交构建 `zed-web.zip` 和 Windows x64 `remote-server.exe`，生成记录源提交、产物大小和 SHA-256 的构建清单。 |
| WheelMaker 发布器及 Release Server | 发布器从独立入口触发 Zed `stable` 的 GitHub 构建，取回产物并核对构建清单。Release Server 在两个产物验证后原子推进 Zed 稳定版指针，发布清单提供下载地址、字节数和 SHA-256。 |
| Gateway | 提供 Zed HTTPS 站点和同源 WebSocket。启动时安装已发布的 Web 包，管理本地可用版本，并将授权编辑连接转发给目标 Hub。 |
| Registry | 管理登录和编辑站点授权。复用 `registry.project.list` 及项目范围校验，绑定账号、Hub、项目和编辑器版本。 |
| Windows Hub 与 `remote-server` | Hub 根据已登记的项目定位本机目录并管理匹配版本的进程。`remote-server` 在目标项目上下文中运行文件服务及语言服务器，并施加文件访问约束。 |

#### 发布、安装和更新

1. 首次部署时，运维安装支持该集成的 WheelMaker Gateway 和 Windows Hub，设置 `registry.zed.publicUrl`，并将域名解析到 Gateway。发布人员点击 WheelMaker 发布页的 **发布 Zed Web** 时，发布器调度 Zed 构建工作流，校验取回的构建清单和两份产物，再独立提交 Zed 组件版本。
2. Gateway 启动时读取 Zed 稳定版清单。已验证的本地包与清单匹配时复用。本地版本缺失或 SHA-256 变化时，将包下载到临时位置，核对长度和 SHA-256，验证入口及其资源后原子切换到新版本。带版本的 JS、Wasm 和资源使用持久缓存，入口页面在使用缓存前重新验证。
3. 更新失败时继续服务最近一次验证成功的本地版本。首次启动且没有可用包时，Zed 入口返回未就绪状态，Gateway 的其他服务可继续处理请求。
4. 启用的 Windows Hub 根据 Gateway 已启用的版本准备匹配产物，离线 Hub 在重连后补齐。打开项目时复用已校验的安装，所需版本缺失时等待同一次下载和校验完成后启动。编辑会话中的页面、资源和进程按同一组件版本配对，保留仍被会话引用的版本。

#### 登录、打开项目和文件访问

1. Registry 在校验配置的回跳地址后，把既有 WheelMaker 登录转换为 Zed 站点的会话。使用短期、单次兑换的登录凭据，在 Zed 站点建立以 HTTPS、`HttpOnly` Cookie 维持的会话。编辑请求和 WebSocket 都校验该会话及其权限，Registry 撤销授权后相应编辑会话失效。
2. 浏览器基于 Registry 返回的项目标识发起打开请求。Registry 复核用户的项目范围和目标 Hub 的状态，授权结果绑定待打开的项目和页面版本。Hub 用本机已登记的路径确定项目根目录。Worker 通过现有共享隧道转发编辑协议，Hub 为获准的工作区会话启动相应进程。关闭会话、授权撤销或断连超时后回收进程及转发资源。
3. 文件服务校验实际文件路径，覆盖 Windows 路径别名、符号链接、目录联接和新文件的父目录。项目根目录提供读写。跳转到根目录外时，通过项目工具链及语言服务器识别的 SDK、标准库和依赖源码获得范围受限的只读访问。其余路径拒绝访问。浏览器提交的项目标识只用于索引 Registry 授权的记录。
4. 目标 Hub 使用项目环境启动语言服务器。补全、悬浮提示、内联提示和跳转结果通过现有 Zed 编辑协议返回。Web 从目标 Hub 获取对应的全局及项目配置，按需加载当前主题和所需资源。

### 预估改动面

- Zed 仓库涉及 Web 登录与工作区切换、标题栏集成、`remote_server` 文件访问边界，以及 Web 和 Windows 可执行文件的配套构建与打包。以 `crates/zed_web_workspace`、`crates/remote_server` 和 `script/windows-local-web` 为探索起点。
- WheelMaker 仓库涉及共享配置和 Gateway 的 Zed 站点、Registry 登录及已登记项目授权、Hub 版本安装和进程生命周期，以及 Release Server 的 Zed 组件发布。优先复用现有 Registry、Gateway、Hub、共享隧道和发布机制。
- 验证完成后更新 WheelMaker 的 `docs/wiki/architecture/gateway.md` 和 `docs/wiki/release-and-build/release.md`，记录域名配置、首次部署、启动更新及失败回退。

## 验收

- 从另一台电脑以 HTTPS 打开配置的 Zed 域名。未登录时可登录并返回编辑器，已登录时可复用会话。用浏览器记录验证回跳、同源会话和标题栏状态。
- 用有权限的账号打开两个不同 Windows Hub 上的已登记项目。通过浏览器验证同标签页切换、未保存内容提示、离线提示及目标连接失败时原项目仍可使用。验证用户只能看到授权项目，项目范围变化或撤销后新连接和被撤销的编辑会话受到限制。
- 用浏览器打开、修改并保存项目文件，在 Hub 核对落盘内容。用启用了语言服务器的 Go 项目验证补全、悬浮提示、内联提示、项目内定义和标准库或第三方依赖定义跳转。验证外部依赖只读，项目范围以外的无关文件无法通过编辑文件接口访问。
- 对照目标 Hub 上的桌面 Zed，检查同一项目的主题、字体和现有 Web 快捷键。打开中文 Markdown 和 TSX 样例检查字形和语法高亮。清空浏览器缓存后记录代表性项目从进入编辑器到工作区就绪的压缩传输量，对照 5 MB 目标。再次打开时检查当前主题和应用资源的缓存命中情况。
- 从 WheelMaker 发布页的 Zed Web 卡片提交发布。验证该操作只提交 Zed Web，并通过 Actions 构建记录和发布清单核对两份产物来自同一次构建，校验通过后才更新 Zed 稳定版。验证 Gateway 启动检查、相同 SHA-256 的下载跳过、新版自动切换和 Hub 配套下载。检查前后端版本一致，离线 Hub 重连后可开项目。模拟下载或校验失败，验证成功版本保留，以及首次安装无缓存时 Zed 的未就绪反馈和其他站点的可用性。
- 并发打开两个浏览器编辑会话，在 Hub 检查对应的进程和路由。关闭、断连超时及撤销其中一个会话时检查该会话的资源回收和另一个会话的文件编辑。
