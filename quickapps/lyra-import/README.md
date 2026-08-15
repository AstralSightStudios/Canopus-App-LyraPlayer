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
    ├── cover.bin                 # 可选，180×180 LVGL v9 ARGB8888
    ├── background.bin            # 可选，336×480 LVGL v9 ARGB8888
    └── lyrics.lrc | lyrics.json  # 可选
```

Lyra Player 原生模块只读以下物理目录，不包含任何联网或 interconnect 代码：

```text
/data/files/com.canopus.lyraimport/lyra
```

`library.json` 在全部资源落盘后通过同目录临时文件发布。发布期间保留旧 manifest 备份，若新文件移动失败则恢复旧文件。Vela 文件 API 不保证覆盖式原子 rename，因此 Lyra Player 会保留上一次成功读取的列表并在短暂缺失时重试。

## 音乐库排序

AstroBox 插件可通过独立的 `lyra-library-*` v1 协议分页读取当前音乐库，并对指定曲目执行相邻“上移/下移”。快应用只返回曲名、歌手、专辑和时长，不暴露沙箱路径。排序请求必须携带由有序曲目 ID 生成的定长修订值；列表已变化、manifest 无效或正在导入时均不会覆盖文件。排序后的 manifest 数组顺序就是 Lyra 的本地列表及上一首/下一首顺序。

使用排序功能时必须同时更新 AstroBox 插件与本快应用；完整消息定义见 `docs/LOCAL_IMPORT_PROTOCOL.md`。

## 构建

```sh
npm install
npm run build
```

## 传输约束

- 协议版本：2
- interconnect 包名：`com.canopus.lyraimport`
- 编码：UTF-8 JSON
- AstroBox → 快应用发送帧上限：49152 字节（48 KiB 单包）
- 快应用 → AstroBox 接收帧上限：8192 字节
- 分块：默认 CRC32 模式最多 35840 原始字节；无校验高速模式最多 36608 原始字节；无校验超高速模式最多 36753 原始字节（48 KiB 单包上限）；
- 分片校验：默认 IEEE CRC32；用户显式选择无校验时省略 `crc32` 字段，但仍严格检查 Base64、长度、asset、seq、offset、ACK 和 staging；旧插件未协商时按 CRC32 兼容处理；
- ACK 窗口：1
- 每块默认使用 IEEE CRC-32；无校验模式省略该字段
- 单次事务：1 个 MP3，可附带 1 张 180×180 BIN 封面、1 张 336×480 BIN 播放页背景和 1 份歌词
- 快应用 hello 声明 `background`、`maxAssets:4` 和 `imageFormats:["lvgl-v9-argb8888-bin"]`；旧插件发送的 JPG/PNG 封面仍兼容
- 最大音频 64 MiB、封面 129612 字节、背景 645132 字节、歌词 2 MiB

完整消息定义见 Lyra Player 仓库的 `docs/LOCAL_IMPORT_PROTOCOL.md`。
