# 受管 Agent 安装入口

Controller 无需配置面板目录即可提供 `/install.sh`、`/install.ps1`。
接入向导生成的 POSIX 命令用于 sh/bash/zsh，Windows 命令直接粘贴到 **PowerShell**。

## 下载目录

在 `RUSTFRP_PUBLIC_URL` 同源的 `/downloads/` 配置静态下载服务（建议 nginx，避免 Controller 整包读入大二进制）。每个平台放置原始可执行文件及对应 `.sha256` 文件：

- `rustfrp-agent-linux-x86_64` / `rustfrp-agent-linux-aarch64`
- `rustfrp-agent-macos-x86_64` / `rustfrp-agent-macos-aarch64`
- `rustfrp-agent-windows-x86_64.exe` / `rustfrp-agent-windows-aarch64.exe`

`.sha256` 内容为 64 位小写 SHA256 十六进制值，允许末尾换行。
例如在构建环境生成：

```sh
cp target/release/rustfrp-agent /srv/rustfrp-downloads/rustfrp-agent-linux-x86_64
sha256sum /srv/rustfrp-downloads/rustfrp-agent-linux-x86_64 | cut -d ' ' -f 1 > /srv/rustfrp-downloads/rustfrp-agent-linux-x86_64.sha256
```

必须部署 HTTPS。POSIX 禁止降级到 HTTP；PowerShell 下载不跟随重定向。脚本下载和 agent 下载不需要管理员登录，不在下载 URL 中加入接入令牌。

## 当前行为与边界

安装到当前用户目录：POSIX `~/.rustfrp/bin/`，Windows `%LOCALAPPDATA%/RustFRP/bin/`。先下载至临时文件、校验 SHA256，再替换 agent；下载失败或校验失败不覆盖已安装 agent。首次安装后在前台接入，身份保存在用户状态目录。一次性令牌不写入启动文件。

安装脚本本身不注册系统服务或开机启动，不把前台进程误报为已安装后台服务；关闭终端会停止该 agent。需要常驻时，用 agent 自带的命令注册为系统服务：

```sh
# 系统级（需要 root；Linux 使用 DynamicUser 与沙箱选项，状态目录默认 /var/lib/<name>）
sudo rustfrp-agent service install -- --mode client --controller panel.example.com
# 当前用户登录后运行，不需要 root（Linux 另需 loginctl enable-linger）
rustfrp-agent service install --user -- --mode client --controller panel.example.com
rustfrp-agent service status        # 退出码：0 运行中，3 未运行，4 未安装
rustfrp-agent service install --print -- ...   # 只打印服务定义，不做任何改动
```

在 `--` 之后写与前台运行完全相同的参数；首次接入的一次性令牌只在第一次启动时使用，之后可从服务定义里删除。Windows 方式（任务计划）尚未在实机验证。已经接入的客户端后续直接用 `--controller` 和相同 `--state-dir` 启动，无需再次使用令牌。

SHA256 用于完整性校验，当前仍信任 HTTPS 下载源，**尚未实现发布签名验证**。三平台发布构建、签名、公证及版本固定仍需后续发布流水线完成。没有相应下载产物时安装会明确失败，不能视为部署完成。

## 验证

`python3 tools/installer/test_install.py` 在临时用户目录里执行真实 sh 脚本，以假网络/agent 验证参数、失败不覆盖及临时文件清理。Rust 测试验证无登录脚本托管、命令特殊字符传递。Windows 需额外在真实 PowerShell 上验证，当前 macOS 环境无 PowerShell，不能声称已实测。

增量内存：脚本用 `include_str!` 放在 Controller 静态只读数据中，无新增后台任务、容器或运行时依赖；下载由外部静态服务器流式提供，不增加 Agent 常驻开销。

## 独立节点

独立 Edge 节点的部署与行为见 `docs/NODES.md`；`service install` 同样适用于节点：`rustfrp-agent service install -- --mode edge --controller … --enroll-token … --public-host … -c …`。
