# 本机会话日志修复

[English](../en/session-log-repair.md)

突然断电或进程崩溃可能留下未写完的 JSONL 尾行。`ternilo repair-session-log` 在本机数据目录离线时检查一个会话；默认只报告拟执行的操作，传入 `--apply` 才会在保存原文件备份后修改日志。

先退出桌面应用并停止对应数据目录的本地服务。RPC／ACP 等独立进程也必须退出；维护命令会取得与应用相同的独占目录锁，目录正在使用时拒绝检查和修改。

```sh
ternilo stop --data-dir /path/to/data
ternilo repair-session-log --data-dir /path/to/data --session-id SESSION_ID
ternilo repair-session-log --data-dir /path/to/data --session-id SESSION_ID --apply
ternilo serve --data-dir /path/to/data
```

`--data-dir` 也可通过 `TERNILO_LOCAL_DATA_DIR` 设置，省略时使用普通客户端的默认数据目录。Session ID 来自工作台会话或自动化接口；命令按 ID 定位日志，无需手工转换文件名。

检查从头验证每条事件及其连续序号。只有以下操作可自动应用：

- 最后一条事件完整且序号正确，但缺少换行：保留事件，补充换行，使后续追加仍为独立记录。
- 最后一个未以换行结束的 JSON 记录被截断：移除该不完整尾行，保留此前所有事件的原始字节。
- 文件已经完整：报告 `none`，不创建备份或改写文件。

完整记录解析失败、空行、事件序号跳跃或无法识别的记录都不会被跳过；命令失败且不修改文件。它不会恢复不完整记录中的输出，也不会重做外部文件操作或自动继续崩溃前的任务。

应用修复前，原始完整文件会保存为同目录下唯一命名的 `*.jsonl.repair-*.bak`，先同步备份再修改原日志；Unix 下备份权限为 `0600`。JSON 报告包含 `action`、`applied`、有效记录数、原始／修复后字节数和 `backup_path`，不包含会话内容。默认检查报告的 `bytes_after` 是拟修复后的长度；`applied` 表示是否实际修改。备份可能含私有对话，应按数据目录原有方式保管。

该命令只用于 `ternilo` 本机及 Node 的原始会话日志。Server 数据库中的同步历史不使用此命令；先修复原电脑再恢复连接，并核对历史与后续事件。已同步且完整的记录不会被重编号。归档也使用原会话日志，修复不会恢复归档或启动任务。
