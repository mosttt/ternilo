# 从 Server 更新电脑上的 Ternilo

在 Server 的“用户设置 → 我的机器”中，选择某台电脑的“更新客户端”，或使用列表顶部的“客户端更新”批量选择。Server 推荐与自身版本一致的正式客户端发行版。更新期间保留配置、会话、电脑身份与凭据；安装重启时电脑会短暂离线。

本功能用于官方 browser／CLI 客户端包，不更新桌面安装器、容器或 Work。首次使用需要手动将 Server 与接入端统一升级到支持 Executor 协议 49 的版本。之后，同一协议及数据代次内的发行版才可远程更新；协议或数据格式变更仍需统一停机升级。

## 在电脑上启用一次

远程更新由电脑所有者明确启用。使用实际运行 Ternilo 的账号，选择与当前启动方式一致的命令；`--data-dir` 必须指向正在使用的实例目录，使用默认目录时可省略。

直接启动的服务：

```sh
ternilo updates enable --restart direct --data-dir "<实例目录>"
```

用户级 systemd 服务：

```sh
ternilo updates enable --restart systemd --service ternilo.service --data-dir "<实例目录>"
```

systemd 主服务必须有持久的 unit 文件；`systemd-run` 临时服务停止后会消失，不能用作客户端的重启目标。更新器自身使用独立的临时 unit，避免随旧主服务停止。

系统级 systemd 服务另外添加 `--system`。该账号必须有权限控制指定服务并创建独立的 systemd 更新单元。Supervisor 使用准确的 program 名称：

```sh
ternilo updates enable --restart supervisor --service ternilo --supervisor-config "<supervisord.conf>" --data-dir "<实例目录>"
```

Windows 当前仅提供 direct 重启。不要把 systemd／Supervisor 管理的服务配置成 direct，否则进程管理器可能在文件交换期间自动重启旧服务。服务管理器的启动命令应使用稳定入口（官网安装脚本创建的 `bin/ternilo`），不要固定到 `releases/<版本>-<平台>/bin/ternilo`。

启用后按原启动方式重启一次 Ternilo，再在 Server 更新弹窗中点击“刷新”。每份安装仅由一个实例管理；同机多实例分别使用独立安装目录。更新还会检查是否有其他服务进程占用同一安装。

## 谁负责下载和安装

Server 从官方 GitHub Release 获取指定版本的 `client-updates.json`、`client-updates.json.sig` 和对应平台的完整客户端包。清单由发行流水线使用专用 Ed25519 私钥签名，包含文件大小、SHA-256、最低客户端版本、Server 版本范围与数据代次。Server 和客户端只持有内置公钥；生产签名私钥不部署到 Server 或客户端。

Server 验签并缓存安装包，再签发仅限这台电脑、这次操作的短期下载授权。电脑通过与 Server 的 HTTPS 连接取得安装包，独立验签和验证包内容。更新命令与进度通过已认证的 WebSocket 连接传输，不另建状态上报通道。生产部署应为 Server 配置 HTTPS。

下载完成后等待对话、子任务、排队任务与后台执行资源全部空闲，随后进入维护状态，暂不接受新任务。独立更新进程先检查新程序能否启动，再停止旧服务、安装完整程序包、按原方式重启并检查本机服务健康。安装成功并重新连接到 Server 后显示完成；单纯断网不会导致程序回退。下载和等待空闲阶段可以取消；安装开始后等待结果。

官网安装脚本创建的版本目录会保留：新版安装到新的 `releases/<版本>-<平台>/bin`，再切换启动脚本与插件链接，保留无参数打开浏览器的行为。手动解压的普通安装目录使用完整文件备份与替换。两种方式都不迁移或覆盖实例数据目录；新程序启动失败时恢复旧程序。

Server 默认把最多 2 GiB 的发行缓存放在 `workspace_root` 同级的 `client-update-cache`。可通过 `TERNILO_SERVER_UPDATE_CACHE` 指定可写目录。容器应为该路径配置可写卷；它是可重建的缓存，不是数据备份。

## 查看状态、停用和恢复

```sh
ternilo updates status --data-dir "<实例目录>"
ternilo updates disable --data-dir "<实例目录>"
```

停用阻止新的远程更新，不中断正在进行的安装。Server 断线时，可在本机查看状态。安装中断留下未完成标记时，服务会拒绝使用不完整的程序包启动。先停止对应的 systemd／Supervisor 服务或直接运行的实例，再执行：

```sh
ternilo updates recover --data-dir "<实例目录>"
```

恢复从独立进程还原旧程序并重新启动服务。如果启动入口已损坏，使用本次更新缓存目录中的 `ternilo-update-helper`（Windows 为 `.exe`）执行上述 `updates recover` 命令。不要删除原版本目录、备份目录或 `<实例目录>/cache/client-updates/`，直到确认更新或恢复完成。

发行准备失败、权限不足或服务名称不匹配时，先查看弹窗诊断并修正对应问题，再重试。不会自动降级、覆盖同名的已有版本目录，或跨数据代次安装；这类情况由本机所有者安排维护。

[电脑管理](computers.md) · [发行与签名流程](ci-release.md)
