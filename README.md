# Cloudflare DDNS

Windows x64 常驻程序：定时获取 Windows 主机的公网 IPv6，并同步到 Cloudflare 中已创建的 AAAA 记录。多个 `[[ddns]]` 配置会独立轮询；公网地址未变化时不会重复更新记录。程序只改记录内容，保留 Cloudflare 当前 TTL 和代理设置。

## 下载运行

每次推送 Git tag，GitHub Actions 会构建 Windows x64 程序并发布 GitHub Release。下载并解压 `cloudflare-ddns-windows-x64.zip`，压缩包包含 `cloudflare-ddns.exe` 和配置模板 `config.toml`。

编辑 `config.toml`，为每个 zone 填入 Cloudflare API Token。然后在 PowerShell 中进入解压目录并运行：

    .\cloudflare-ddns.exe

程序默认从 exe 所在目录读取 `config.toml`，也可以通过参数指定配置路径：

    .\cloudflare-ddns.exe D:\path\to\config.toml

程序会持续运行，按 Ctrl+C 停止。可通过 Windows 任务计划程序配置登录后启动，并设置失败后重新启动。

## 配置要求

配置文件已包含这两个 zone 和对应记录：

* `example.com`
* `example2.com`

请填写 `api_token`。Token 需要对应 Zone 的 DNS 编辑权限；请先在 Cloudflare 创建配置中列出的 AAAA 记录，程序不会自动创建记录。`interval_secs` 单位为秒，最小实际轮询间隔为 10 秒。

## 从源码构建

仅支持 64 位 Windows MSVC：

    cargo build --release --locked --target x86_64-pc-windows-msvc

生成文件位于 `target\x86_64-pc-windows-msvc\release\cloudflare-ddns.exe`。
