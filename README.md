# Lyra Player

Canopus 原生音乐播放器模块。构建时需要同级的 `Canopus` 框架仓库，
也可通过 `CANOPUS_ROOT` 指定框架目录。

## Prod 安装表盘

```sh
scripts/build-install-watchface-prod.sh xiaomi-band-11
```

不指定设备时分别构建 10 Pro（`.036`、`.043` 同目录）与 Band 11（`.139`）安装包。
也可传入一个或多个完整 target ID，或设置 `CANOPUS_DEVICE` / `CANOPUS_TARGET`；
脚本参数优先，其次 `CANOPUS_DEVICE`。选择单个版本会重建对应设备包，保留其他设备目录。
脚本会交叉编译、运行 ELF verifier、签名 CMI1 凭证，并从框架共享模板生成 prod Lua。
它复用 BluetoothAudio 的安装协议以及框架的 UI 进度组件。
外部安装 Lua 仅用普通 `io.open` 访问 `/canopus/install`，不包含 execute 恢复或 debug 操作。
每次写入及回读、校验和安装提交都有 UI 状态提示。

产物目录为 `watchfaces/lyra-player-prod/xiaomi-band-10-pro/` 与
`watchfaces/lyra-player-prod/xiaomi-band-11/`，各设备目录根部仅有一个 `main.lua` 与 `.bin` 资源。
将对应设备目录交给表盘打包器；子目录 `build/` 与 `docs/` 不参与打包。
先更新 Canopus 框架安装表盘；旧 Supervisor 已常驻时须重启后运行新版，
由框架准备 `/data/canopus/inbox` 和新安装端点。安装后的模块保持禁用。
`LYRA_PLAYER_ICON` 可以指定应用图标 PNG；私钥不会进入安装包。

**Band 11 `.139` 目前仅完成构建与安装入口适配。** Rust 后端仍使用
`static_candidate.rs`，identity guard 返回 `-38`，不能启用为可工作的播放器。
框架管理器的实机成功不代表 Lyra 音频服务已有实机支持。

10 Pro 的开发安装流程和播放控制图标说明见
[开发安装表盘](watchfaces/lyra-player/README.md)。
