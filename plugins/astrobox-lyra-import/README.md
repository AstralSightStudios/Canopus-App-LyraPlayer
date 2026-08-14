# AstroBox Lyra Import

AstroBox NG API level 3 插件，用于把本地 MP3 或网易云音乐导入 Vela 快应用 `com.canopus.lyraimport`。

## 功能

- 选择已连接的小米穿戴设备；
- 本地导入 MP3，并可附带 JPG/PNG 封面和 LRC/JSON/TXT 歌词；
- 网易云扫码登录或手动填写 Cookie；
- 搜索网易云歌曲；
- 下载歌曲音频、专辑封面和原文/翻译歌词；
- 按 `LOCAL_IMPORT_PROTOCOL.md` v2 使用 4096 原始字节 Base64 分片、CRC32 和单窗口 ACK 发送；
- AstroBox 与手表快应用两端同时显示实时总进度；
- 只向 `com.canopus.lyraimport` 发送并监听消息，不再使用系统 FetchBridge 包名。

## 数据流

```text
本地文件 / 网易云 EAPI
          ↓
AstroBox Lyra Import WASM 插件
          ↓ interconnect (com.canopus.lyraimport)
Vela Lyra Import 快应用
          ↓ internal://files/lyra
Lyra Player 原生只读播放器
```

WASI 文件选择器会先把本地文件复制到插件的 `media/` 目录。网络音频使用 Waki 分块读取并写入临时文件，再由导入状态机增量发送；不会把完整 MP3 常驻内存。

## 构建

```sh
python3 scripts/build_dist.py --release --package
```

输出位于 `dist/`。
