# AstroBox Lyra Import

AstroBox NG API level 3 插件，用于把本地 MP3 或网易云音乐导入 Vela 快应用 `com.canopus.lyraimport`。

## 功能

- 选择已连接的小米穿戴设备；
- 本地导入 MP3，并可附带 JPG/PNG 封面和 LRC/JSON/TXT 歌词；封面会在插件内预处理为 180×180 LVGL v9 ARGB8888 BIN，并从同一封面生成 336×520 混色模糊背景（底部 40px 透明渐隐）；
- 网易云扫码登录，并将登录状态保存在插件私有目录；
- 搜索网易云歌曲，并以卡片列表直接选择导入；
- 加载个人歌单，查看歌单歌曲并直接导入；
- 下载歌曲音频、专辑封面和原文/翻译歌词；
- 按 `LOCAL_IMPORT_PROTOCOL.md` v2 使用 Base64 分片：CRC32 模式 35 KiB、无校验高速模式 36608 原始字节、无校验超高速模式 36753 原始字节（48 KiB 单包上限）；默认启用 CRC32，损坏块最多原位重传 2 次；插件 UI 按 ACK 确认字节显示实时 `k/s` 速度；
- AstroBox 与手表快应用两端同时显示实时总进度；
- 分页读取手表当前音乐库，并通过带修订值校验的“上移/下移”操作调整 Lyra 播放顺序；
- 只向 `com.canopus.lyraimport` 发送并监听消息，不再使用系统 FetchBridge 包名。

## 数据流

```text
本地文件 / 网易云 API（WEAPI 登录、EAPI 数据接口）
          ↓
AstroBox Lyra Import WASM 插件
          ↓ interconnect (com.canopus.lyraimport)
Vela Lyra Import 快应用
          ↓ internal://files/lyra
Lyra Player 原生只读播放器
```

WASI 文件选择器会先把本地文件复制到插件的 `media/` 目录。网络音频使用 Waki 分块读取并写入临时文件，再由导入状态机增量发送；不会把完整 MP3 常驻内存。JPEG/PNG 只在生成固定尺寸的 BIN 时解码，旧版快应用未声明图片能力时会自动省略这些图片资源。

“手表音乐”使用独立的 `lyra-library-*` v1 管理协议，每页请求 8 首且不传输文件路径。排序请求携带最近一次完整读取的有序列表修订值；发生冲突时不会覆盖手表音乐库，用户刷新后即可重试。排序需要同时安装最新版插件和快应用。

## 构建

```sh
python3 scripts/build_dist.py --release --package
```

输出位于 `dist/`。
