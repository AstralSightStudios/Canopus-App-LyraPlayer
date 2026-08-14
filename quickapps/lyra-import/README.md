# Lyra Import 快应用

包名：`com.canopus.lyraimport`

该 Vela 快应用是 Lyra Player 的唯一手机通信端点。AstroBox 的 Lyra Import 插件把本地音乐或网易云音乐资源发送到本快应用；快应用负责分块接收、显示实时进度，并写入自己的 `internal://files` 沙箱。

> **请勿在导入音乐后删除本快应用。** 快应用的 `internal://files` 属于应用私有数据，卸载 `com.canopus.lyraimport` 时，已经导入的音频、封面、歌词和音乐库也会被一并删除。

## 存储布局

```text
internal://files/lyra/
├── library.json
├── staging/<transaction-id>/
└── tracks/<transaction-id>/
    ├── audio.mp3
    ├── cover.jpg | cover.png       # 可选
    └── lyrics.lrc | lyrics.json    # 可选
```

Lyra Player 原生模块只读以下物理目录，不包含任何联网或 interconnect 代码：

```text
/data/files/com.canopus.lyraimport/lyra
```

`library.json` 只会在全部资源落盘后原子替换，因此 Lyra Player 不会看到半成品导入。

## 构建

```sh
npm install
npm run build
```

## 传输约束

- 协议版本：2
- interconnect 包名：`com.canopus.lyraimport`
- 编码：UTF-8 JSON
- 分块：`base64`，每块最多 4096 个原始字节
- ACK 窗口：1
- 每块校验：IEEE CRC-32
- 单次事务：1 个 MP3，可附带 1 张封面和 1 份歌词
- 最大音频 64 MiB、封面 4 MiB、歌词 2 MiB

完整消息定义见 Lyra Player 仓库的 `docs/LOCAL_IMPORT_PROTOCOL.md`。
