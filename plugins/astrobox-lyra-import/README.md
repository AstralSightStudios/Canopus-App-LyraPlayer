# AstroBox Lyra Import

AstroBox NG API level 4 插件：把本地 MP3 或网易云音乐通过 CanopusConnect 直接导入手表上的 Lyra Player
（Canopus 模块 `lyra_player`）。音乐以二进制数据流写入手表，不需要快应用，也不经过 interconnect。

需要手表固件的 Canopus 目标提供 `ctx->connect`：Band 11 `.155`、`.179` 与 Band 10 Pro `.043` 已在 Mirage 验证。
其他固件（`.036`、`.139`）按 ADR-CAN-009 没有它，播放器照常运行，但不能导入。

## 功能

- 选择已连接的小米穿戴设备，连接后向模块 `lyra_player` 发 `HELLO`，读到手表屏幕尺寸与曲库修订号；
- 本地导入 MP3，并可附带 JPG/PNG 封面和 LRC/JSON/TXT 歌词。文件按内容识别类型，不依赖后缀
  （安卓选择器只给无后缀的 content URI 名称）：MP3 以 MPEG Layer III 帧头判断（跳过 ID3v2），
  封面以 JPEG/PNG 文件头判断，歌词按内容区分 JSON 与 LRC；选择 MP3 后从 ID3v2/ID3v1 预填曲名、歌手、专辑；
- 封面在插件内缩放为 176×176 圆角 LVGL v9 ARGB8888 BIN，并取封面上下两种颜色作为播放页背景渐变的色标；
  MP3 时长由 `mp3-duration` 读取；
- 网易云扫码登录，登录状态保存在插件私有目录；搜索歌曲、加载个人歌单并直接导入；
  下载歌曲音频、专辑封面和原文/翻译歌词（翻译行合并进 LRC）；
- 传输由 `crates/lyra-transfer` 的发送状态机完成：16 KiB 窗口、手表每 4 KiB 回报进度、乱序时按手表给出的偏移回退重发、
  每份资源 CRC32 校验；界面显示总进度和实时速度，手表 45 秒没有回报进度就判为停滞并取消；
- “手表音乐”分页读取手表曲库（`LIST`），可上移/下移（`MOVE`，带修订号校验，冲突时不覆盖）、删除（`DELETE`）。

## 数据流

```text
本地文件 / 网易云 API（WEAPI 登录、EAPI 数据接口）
          ↓
AstroBox Lyra Import WASM 插件（权限 canopusconnect.send / canopusconnect.listen / device）
          ↓ CanopusConnect 二进制消息，每条 ≤ 480 字节
Canopus Supervisor → lyra_player 模块 → /data/canopus/m.lyra_player/
```

WASI 文件选择器会先把本地文件复制到插件的 `media/` 目录。网络音频用 Waki 分块读取并写入临时文件，
再增量发送；不会把完整 MP3 常驻内存。JPEG/PNG 只在生成固定尺寸的 BIN 时解码。

协议细节见仓库的 [docs/CONNECT_PROTOCOL.md](../../docs/CONNECT_PROTOCOL.md)。

## 构建

```sh
python3 scripts/build_dist.py --release --package        # wasm32-wasip2，输出 dist/*.abp
cargo test --target aarch64-apple-darwin                 # 主机测试（把目标换成本机三元组）
```

输出位于 `dist/`。插件的 WIT 在 `wit/`：`deps/astrobox-psys-host-v4.wit` 与 `astrobox-psys-plugin-v4.wit`
定义了 `canopusconnect` 接口（`send` / `listen` / `unlisten` 与 `on-message` 回调）。
