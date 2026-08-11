# Lyra Player 安装表盘

这是一个一次性 Canopus 模块安装表盘。安装并打开表盘后，它会把经过
CMI1 Ed25519 签名的 Lyra Player ELF 和 receipt 写入
`/data/canopus/inbox/`，再通过 `/dev/canopus` 请求 supervisor 安装模块。
模块安装后默认禁用，必须在 Canopus Manager 中审核并启用。

## 前置条件

设备上必须已经安装 Canopus supervisor 和原生 Manager，且
`/dev/canopus` 可用。目标固件必须与构建时选择的 target pack 完全一致。

## 构建并签名

默认构建 Band 10 Pro 3.101.030：

```sh
scripts/build-install-watchface.sh
```

选择其他已支持目标：

```sh
CANOPUS_TARGET=xiaomi-band-10-pro-3.101.036 \
  scripts/build-install-watchface.sh

CANOPUS_TARGET=xiaomi-band-9-pro-3.1.175 \
  scripts/build-install-watchface.sh
```

构建流程会：

1. 使用对应 target feature 交叉编译 `lyra-player-device`。
2. 加入 NuttX modlib constructor/destructor shim，链接为 ELF32 ET_REL。
3. 使用 Canopus verifier 校验目标、重定位和固件地址。
4. 从 `<CANOPUS_ROOT>/.canopus-local/module-installer-ed25519.pem` 复制一份
   权限为 `0600` 的临时签名密钥，在临时目录中完成 CMI1 receipt 签名，
   随后立即删除临时目录。
5. 将 `module.bin` 与 `receipt.bin` 放入本目录。

本地私钥不会复制到输出目录、表盘资源或 Git。可通过以下环境变量覆盖：

- `CANOPUS_ROOT=/path/to/Canopus`
- `MODULE_INSTALL_KEY=/secure/path/module-installer-ed25519.pem`
- `CANOPUS_TARGET=<target-id>`
- `CANOPUS_BUILD_OUT=/path/to/build-output`
- `CANOPUS_WATCHFACE_OUT=/path/to/watchface-output`

`module.bin` 和 `receipt.bin` 是按目标固件生成的构建产物，不应跨固件复用。

## 安装

1. 构建当前设备精确固件对应的安装表盘。
2. 将整个 `watchfaces/lyra-player` 目录作为普通表盘打包并安装。
3. 打开表盘一次，等待显示安装结果。
4. 在 Canopus Manager 中启用 `lyra_player`。
5. 按 Manager/Canopus installer 的流程重启并执行 LOAD。
6. 依次完成原生应用发布阶段，使 Lyra 页面和 Launcher 入口生效。

失败时表盘会保留并显示诊断信息。receipt 会锁定 target ID、固件
SHA-256、模块长度和模块 SHA-256；任一不匹配时 supervisor 应拒绝安装。
