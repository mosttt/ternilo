# 工具失败与只读验证

## 具名检查与权限

`shell-tool` 的 `checks` 配置登记固定的名称、命令和超时；非空配置注册 `run_check`，工具 Schema 列出名称，调用不接受命令或完全访问参数。角色说明在派发前指出只读工具边界，子代理继承配置。Local Shell 的 `read_only` 请求显式强制沙箱，不受主会话完全访问影响；旧请求缺省为 false，保留原 Shell、Git、Hook 和后台任务入口。

只读是工作区写入边界，不把预先信任的测试代码变成无副作用程序。现有本地网络边界不变，Worker 外层沙箱不能提供此模式时明确拒绝。

## 进程专属临时目录

受限 Shell、后台任务和持久终端创建专属临时目录，并传入临时／缓存环境。目录与已有执行租约一起持有，完成进程树清理后释放；取消调用不提前删除。Linux 在私有 `/tmp` 后重新挂载工作区，保留 `/tmp` 下工作区的可见性和原写权限。Seatbelt 使用规范路径；系统工具 shim 的缓存文件不受 `TMPDIR` 控制，由 `getconf DARWIN_USER_TEMP_DIR` 查询并规范化目录后，只允许其 `xcrun_db` 文件及随机后缀缓存文件写入；Windows runner 只为宿主指定的 scratch 目录增加写权限，原读取隔离边界不变。

## Rhai 失败结果与图谱

`try_tool` 与原 SDK 共用一次 dispatch，保存结构化工具结果，返回 `ok / result / error`。普通 `tools::name`／`call_tool` 继续抛错，错误摘要从 Shell／Git 的退出状态和首条诊断提取；完整 JSON 保留在子调用事件中。权限、Hooks、调用上限、输出预算和取消重新准入仍走原路径。Shell 超时返回已捕获的输出，仍先完成进程树回收。

图谱默认仅输出所选文件内已声明符号的候选调用，并给出省略数量；完整图谱由 `detail: full` 获取，影响查询仍使用完整解析结果。Rhai 解析错误提供有界源码片段；包含反引号时说明字符串定界符，保留解析失败不执行的语义。

## 验证状态

Local 实际 reviewer 测试通过：标准库单元测试成功，工作区写入、普通 Shell、未知名称和额外命令被拒绝，完全访问主会话仍不能借检查写文件；超时输出和临时目录清理保留，重启后配置可用。非 Git 目录的结构化结果与简短异常、图谱精简及完整视图通过。Rhai 13 项测试通过，包含取消、重新准入、结构化失败继续执行及反引号解析提示。

严格 all-targets Clippy、最终版本的本机沙箱 5 项、命令捕获 4 项、持久终端生命周期 3 项、Code Mode 5 项、图谱 4 项及角色策略 1 项通过。客户端／Server 匹配构建通过。

配置相关前端 24 项、TypeScript、i18n 与生产构建通过。Local 与 Server 接入电脑的真实浏览器验收分别为 41.13 秒和 41.31 秒，覆盖中英韩配置保存、集中／独立视图同步、实际输入框提交检查、恢复继承及 390px 手机布局。截图已检查，页面、控制台与 HTTP 无非预期错误。验收中修复关闭状态的 Tooltip 残留：取消退场动画，闭合内容立即隐藏。模型连接浏览器合同另通过 15.38 秒。

239 份 Markdown 与发行元数据检查通过。Windows、两个 macOS 架构和 Linux ARM 的原生检查由既有 Checks 执行；正式发行和公开程序验收结果另记于对应版本的发行验收。


## 发行检查补充

首轮 Apple Silicon 原生测试确认：只读 scratch 可写、工作区写入被拒，但系统 Python 的 xcrun shim 仍尝试写入用户级 `T/xcrun_db-*`，不使用进程的 `TMPDIR`。`xcrun_nocache=1` 也未阻止可执行程序 shim 的缓存写入，因此移除该无效设置。保留原“标准错误为空”断言，改为读取系统用户临时目录，对规范路径下的缓存文件使用精确正则授权；同时验证其他临时文件仍不可写。两个 macOS 架构继续按实际程序验证。

依赖扫描新报告 [RUSTSEC-2026-0391](https://rustsec.org/advisories/RUSTSEC-2026-0391)、[0398](https://rustsec.org/advisories/RUSTSEC-2026-0398)、[0403](https://rustsec.org/advisories/RUSTSEC-2026-0403) 和 [0410](https://rustsec.org/advisories/RUSTSEC-2026-0410)。Salvo 0.95／0.96 依赖的 zstd 0.13 无法解析到已修复版本，因此改用 Salvo 1.0.1，将依赖更新为 zstd 0.14.3、zstd-safe 8.2.0 和 zstd-sys 2.1.2。保留原审计策略，不为这些公告增加忽略项；HTTP／流式交互随依赖更新重新验收。


Cloud catalog 修订随共享工具 Schema 更新为 v3；执行 policy、可运行 envelope、恢复夹具和浏览器夹具同步引用该修订。首轮全量检查发现示例仍为 v2，已补齐同一合同的调用方；不增加 Work 执行功能，也不自动改写运营者私有 policy。
