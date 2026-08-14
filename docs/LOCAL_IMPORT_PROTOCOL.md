# Lyra Import Protocol v2

本文定义 AstroBox `astrobox-lyra-import` 插件与 Vela 快应用 `com.canopus.lyraimport` 之间的导入协议。快应用工程位于本仓库的 `quickapps/lyra-import`。Lyra Player 原生模块不参与该协议，也不再包含联网、FetchBridge 或 interconnect 功能。

## 1. 架构

```text
本地文件 / 网易云音乐
          │
          ▼
AstroBox Lyra Import 插件
          │  interconnect: com.canopus.lyraimport
          ▼
Vela Lyra Import 快应用
          │  internal://files/lyra
          ▼
/data/files/com.canopus.lyraimport/lyra
          │  只读扫描
          ▼
Lyra Player 原生本地播放器
```

快应用必须保持前台运行，以便显示实时导入进度并接收数据。插件不再监听或发送 `com.xiaomi.miwear.interconnect`。

## 2. 存储发布规则

快应用将事务写入：

```text
internal://files/lyra/staging/<id>/
```

全部资源接收完成后移动到：

```text
internal://files/lyra/tracks/<id>/
```

随后原子替换 `internal://files/lyra/library.json`。原生播放器只读取最终目录和 manifest，不会观察 staging 中的半成品。

manifest schema：

```json
{
  "version": 1,
  "tracks": [
    {
      "id": 347230,
      "name": "歌曲名",
      "artists": [{ "id": 0, "name": "歌手" }],
      "album": {
        "id": 0,
        "name": "专辑",
        "cover_url": "/data/files/com.canopus.lyraimport/lyra/tracks/<id>/cover.jpg"
      },
      "duration_ms": 240000,
      "local_path": "/data/files/com.canopus.lyraimport/lyra/tracks/<id>/audio.mp3",
      "lyrics_path": "/data/files/com.canopus.lyraimport/lyra/tracks/<id>/lyrics.lrc"
    }
  ]
}
```

## 3. 传输约束

- 协议版本：`2`
- 消息编码：UTF-8 JSON
- 分片数据编码：Base64
- 快应用最大分片：4096 原始字节
- ACK window：1
- 分片校验：IEEE CRC-32，小写 8 位十六进制
- 音频上限：64 MiB
- 封面上限：4 MiB
- 歌词上限：2 MiB
- 必须包含一个 `audio` asset；`cover` 和 `lyrics` 可选且各最多一个

## 4. 握手

插件发送：

```json
{"tag":"lyra-import-hello","version":2}
```

快应用回复：

```json
{
  "tag":"lyra-import-hello",
  "version":2,
  "maxChunkBytes":4096,
  "window":1,
  "encodings":["base64"],
  "assets":["audio","cover","lyrics"]
}
```

## 5. 开始事务

```json
{
  "tag":"lyra-import-begin",
  "version":2,
  "id":"transaction-id",
  "track":{
    "id":347230,
    "name":"歌曲名",
    "artists":["歌手"],
    "album":"专辑",
    "albumId":0,
    "durationMs":240000
  },
  "assets":[
    {"kind":"audio","size":1234567},
    {"kind":"cover","size":45678,"extension":"jpg"},
    {"kind":"lyrics","size":3210,"format":"lrc"}
  ]
}
```

资源按 `assets` 顺序逐个发送。快应用回复当前资源：

```json
{
  "tag":"lyra-import-ready",
  "id":"transaction-id",
  "asset":"audio",
  "nextSeq":0,
  "nextOffset":0,
  "maxChunkBytes":4096,
  "window":1
}
```

## 6. 发送分片

```json
{
  "tag":"lyra-import-chunk",
  "id":"transaction-id",
  "asset":"audio",
  "seq":0,
  "offset":0,
  "encoding":"base64",
  "crc32":"9a7c12ef",
  "data":"SUQzBAA="
}
```

快应用写入成功后 ACK：

```json
{
  "tag":"lyra-import-ack",
  "id":"transaction-id",
  "asset":"audio",
  "nextSeq":1,
  "nextOffset":5,
  "receivedBytes":5,
  "totalBytes":1282455
}
```

插件只有收到 ACK 后才能发送下一块。

## 7. 结束资源与提交

资源全部发送后：

```json
{"tag":"lyra-import-asset-end","id":"transaction-id","asset":"audio"}
```

若仍有下一个资源，快应用发送对应 `lyra-import-ready`。全部资源完成后：

```json
{"tag":"lyra-import-assets-done","id":"transaction-id"}
```

插件提交：

```json
{"tag":"lyra-import-commit","id":"transaction-id"}
```

成功回复：

```json
{"tag":"lyra-import-done","id":"transaction-id","track":{}}
```

## 8. 取消与错误

取消：

```json
{"tag":"lyra-import-cancel","id":"transaction-id"}
```

错误：

```json
{
  "tag":"lyra-import-error",
  "id":"transaction-id",
  "code":"checksum-mismatch",
  "message":"chunk CRC32 mismatch"
}
```

收到错误、超时或连接断开后，插件必须停止当前 ACK 链。快应用删除对应 staging 目录；已经发布的旧音乐库不受影响。
