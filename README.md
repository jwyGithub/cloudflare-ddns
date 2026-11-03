# Cloudflare DDNS

定时获取容器所在主机的公网 IPv6，并同步到 Cloudflare 中已创建的 AAAA 记录。多个 DDNS 配置会各自独立轮询；IP 未变化时不会调用更新接口。只更新记录内容，保留 Cloudflare 当前 TTL 和代理状态。

## 配置与运行

复制 config.example.toml 为 config.toml，填写 Cloudflare API Token，然后启动：

    cp config.example.toml config.toml
    docker compose up -d

Token 至少需要对应 Zone 的 DNS 编辑权限。请先在 Cloudflare 创建 records 中列出的 AAAA 记录；程序不会自动创建记录。config.toml 不会被 Git 跟踪。

Compose 使用宿主网络，使容器直接使用宿主机的 IPv6 出站网络。单次轮询失败会记录日志，并在下一轮重试；配置的轮询间隔小于 10 秒时按 10 秒执行。

查看日志：

    docker compose logs -f cloudflare-ddns

## 发布镜像

推送任意 Git tag 会通过 GitHub Actions 构建 linux/amd64 与 linux/arm64 镜像并推送到 GHCR。例如：

    git tag v1.0.0
    git push origin v1.0.0

镜像地址为 ghcr.io/jwygithub/cloudflare-ddns:<tag>。首次发布后，若仓库的 GHCR package 为私有，请在 package 设置中将其设为公开，或先在部署主机登录 GHCR。
