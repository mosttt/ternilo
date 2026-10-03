# 修改与恢复原生账号密码

[English](../en/account-recovery.md) · 简体中文

已登录的原生账号可以从“用户设置 → 通用 → 账号 → 修改密码”输入当前密码、新密码和确认密码。成功后包括当前页面在内的本站原生／OIDC 浏览器登录全部失效，需重新登录。密码更新与会话撤销在同一事务中完成；错误密码或并发改密不会覆盖当前密码。仅有 OIDC 登录的账号不显示此入口。

接口为 `POST /api/v1/auth/password`，使用当前账号的浏览器认证，JSON 字段为 `current_password` 和 `new_password`，不接受目标账号参数。响应不包含密码；该操作不退出外部身份提供方的登录，不撤销电脑凭据、模型 Key 或服务账号凭据。

能够读取 Server 私有配置的运维人员可以重置已有原生账号的密码。账号 ID、角色、个人空间、工作区、会话、电脑与模型授权保持原有归属。此命令不增加账号，也不为仅有 OIDC 登录的账号创建密码。

在运行 Server 的机器上执行：

```bash
./bin/ternilo-server admin reset-password \
  --config-dir /path/to \
  --username owner
```

终端会隐藏输入新密码，并要求重复确认。新密码沿用注册规则：8～1024 字节，空格属于密码。成功后可用原用户名和新密码登录，不需要重建数据库或重启 Server。

自动化场景使用 `--password-stdin`，例如从受保护的临时文件读取：

```bash
./bin/ternilo-server admin reset-password \
  --config-dir /path/to \
  --username owner \
  --password-stdin < /path/to/protected-password-file
```

标准输入会去掉一处末尾 LF／CRLF，其余字符保留；没有终端且未指定该选项时，命令明确拒绝。密码不作为命令参数接收，也不会写入输出和审计。私有配置与密码文件仍应只对运维人员可读。

密码更新与旧的本站原生／OIDC 浏览器会话撤销在同一事务中完成。旧密码在重置前已验证、但尚未签发会话的登录也会被拒绝。旧会话对后续请求失效，网页的在线连接在复核后要求重新登录；已接受的任务不会因此自动中断。

电脑凭据和模型 Key 不随密码重置撤销，OIDC 绑定关系保持；外部 IdP 的凭据与组织会话由 IdP 管理。被封禁或待审核的账号仍保持原状态，已移除账号不能恢复。需要调整访问状态时使用[账号管理](platform-management.md)。

已登录账号可自助改密；忘记密码时使用上述运维恢复。邮件找回与 MFA 尚未提供。若错误的 OIDC／Turnstile 设置阻止登录，参照[登录设置恢复](server-authentication.md)，它与密码重置是两个不同的维护操作。
