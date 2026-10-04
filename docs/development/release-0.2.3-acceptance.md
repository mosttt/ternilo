# v0.2.3 公开发行验收

来源提交 `bd2f5e77d0be751c0cea2000b25ba0d36391fce0`，固定 Linorun 见 `.github/linorun-revision`。[Release](https://github.com/mosttt/ternilo/releases/tag/v0.2.3) 于 2026-10-04T13:10:57Z（北京时间 2026-10-04 21:10:57）公开，已标记为最新稳定版本；[发行工作流](https://github.com/mosttt/ternilo/actions/runs/37198546818)及 [main 检查](https://github.com/mosttt/ternilo/actions/runs/37198505503)均成功。

本轮完成 Server 直接 `serve` 后的 Key 保护网页设置、SQLite／PostgreSQL 选择、管理员创建、独立终端 `setup`，以及初始化／登录／工作台／本机 Ternilo 的浏览器语言默认与手动覆盖。中英文首页、文档索引、快速开始和部署说明重整，正式命令不使用 `./bin/` 前缀。

## 文档、首页与分支

145 份文档链接、双语配对、首页 HTML 导航、发行版本与元数据检查通过。GitHub 实际浏览器检查中英文桌面／手机页面，Logo、四个徽章和导航加载正常。About、官网及 7 个主题标签通过仓库 API 和实际页面确认，重点为开源、本机文件与工具、手机接续、多模型与插件。

确认提交均为 main 祖先后，删除 13 个本地分支、12 个远端分支及 5 个干净工作树；远端删除使用明确的预期 SHA，保留 main 和版本标签，无未合并提交或开放 PR。

## 程序包验收

Linux x64 客户端与 Server 从本次工作流的实际产物中取得，逐份 SHA-256 通过。各自 94 份正式文档／中英文首页与发行源码逐字节一致，不含开发记录。包内版本均为 0.2.3。

包内客户端通过 2 项真实浏览器语言测试；包内 Server 通过 4 项真实浏览器测试，覆盖 SQLite、受限 PostgreSQL、错误 Key 先于数据库连接、失败重试、完整业务路由切换、私有配置权限、初始化 Key 失效／未完成重启更换 Key、原账号保留，以及中文／英文／不支持语言默认值与手动覆盖。

12 个公开客户端／Server 归档全部取得并本地计算 SHA-256，与 GitHub 上传摘要和独立校验文件一致。检查 Linux ELF、Windows PE、macOS Mach-O 的实际架构、程序集合及双语文档／首页内容；Windows 的 CRLF 仅在文本比较时归一化，原始归档哈希仍按原始字节验证。均不包含开发记录或运行时私有配置。

Desktop 的构建、Windows 子系统、ARM64 原生持久化和后台生命周期结果以本次对应 CI 为证，不写成本机已经运行全部物理设备或完整下载全部大型桌面安装器。

## 公开交付

42 项附件：12 个客户端／Server 归档及各自 12 个校验文件、9 个桌面安装器、SOURCE、SHA256SUMS、基础／PostgreSQL Compose、PostgreSQL 初始化 SQL、两份部署指南及两份镜像元数据。六个目标为 Linux x64／ARM64、Windows x64／ARM64 和 macOS Intel／Apple Silicon。公开 SOURCE 与发行提交和固定 Linorun 一致，全部附件清单、GitHub 上传摘要及总校验清单对应。

使用空的独立 Docker 凭据目录完成匿名拉取，核对 AMD64 镜像版本、来源、许可证；公开索引与发行元数据一致，摘要为 `sha256:5e48c4f31f1f5628774505d8f33304a76aaa6fa6b7e8352f25a956f5c1c409ca`。AMD64 摘要为 `sha256:398e604a63191e9796266f41a5ce8138baa3594d5ce6b933d203c4a9472a60da`，ARM64 为 `sha256:088f58e12b0896add487453dcbaeaf7a3b3efa393c3356af3f11f7636691c25a`。两个架构均由原生 Runner 验收，本机运行 AMD64。

公开下载的基础 Compose 与 PostgreSQL 叠加配置均通过真实 Chromium 验收：直接 `up` 后受 Key 保护的网页设置、英语默认／手动中文、登录、只读根目录、Server UID 10001、账号和配置在 down／up 后保留，控制台和 HTTP 错误为零。PostgreSQL 的 owner／app 无 superuser、CREATEDB、CREATEROLE 或 BYPASSRLS，数据库不发布宿主机端口，Server 配置不含 PostgreSQL 管理员密码。

本机 Docker 默认地址池已满，验收临时分配经检查不冲突的独立网段。这只是本机验证条件，不进入项目标准部署配置／CI。两个独立项目的容器、网络和数据卷在验收后清理，用户运行实例保持原版本。

Desktop 发布者签名／公证／自动更新、资源迁移／跨主机接管／故障转移及进一步容量验证仍为后续事项。电脑凭据轮换按用户要求后移，Work 仅保留独立设计。
