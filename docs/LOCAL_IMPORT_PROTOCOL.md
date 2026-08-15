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
        "cover_url": "/data/files/com.canopus.lyraimport/lyra/tracks/<id>/cover.bin",
        "background_url": "/data/files/com.canopus.lyraimport/lyra/tracks/<id>/background.bin"
      },
      "duration_ms": 240000,
      "local_path": "/data/files/com.canopus.lyraimport/lyra/tracks/<id>/audio.mp3",
      "lyrics_path": "/data/files/com.canopus.lyraimport/lyra/tracks/<id>/lyrics.lrc"
    }
  ]
}
```

manifest schema 中 `cover_url` 与 `background_url` 都可以为空；旧版 manifest 缺少 `background_url` 时按空字符串处理。

背景图作为播放页 root 的底层对象显示，不计入普通纵向控件流；Band 10 Pro 3.101.030/3.101.036 使用上述 LVGL v9 BIN，Band 9 Pro 3.1.175 不创建图片控件，也不需要 LVGL v8 图片编码。

## 3. 传输约束

- 协议版本：`2`
- 消息编码：UTF-8 JSON
- 分片数据编码：Base64
- AstroBox → 快应用发送帧上限：49152 字节；CRC32 模式最多 35840 原始字节，无校验高速模式最多 36608 原始字节，无校验超高速模式最多 36753 原始字节，经 Base64/JSON 包装后低于 48 KiB 单包上限
- 快应用 → AstroBox 接收帧上限：8192 字节；音乐库响应按 UTF-8 字节数动态缩页
- 快应用最大分片：CRC32 模式 35840 原始字节（35 KiB）；无校验高速模式 36608 原始字节；无校验超高速模式 36753 原始字节（48 KiB 单包上限）
- ACK window：1（严格停等，保持乱序安全）
- 分片校验：默认 IEEE CRC-32，小写 8 位十六进制；用户显式选择无校验时省略 `crc32` 字段以减少开销，但仍保留 Base64、长度、资源、seq、offset、ACK 和 staging 校验
- 音频上限：64 MiB
- 封面上限：4 MiB；新插件会把 JPEG/PNG 预处理为未压缩 LVGL v9 ARGB8888 BIN，固定为 180×180（129612 字节）
- 背景上限：645132 字节；新插件从封面生成混色模糊 LVGL v9 ARGB8888 BIN，固定为 336×480
- 歌词上限：2 MiB
- 必须包含一个 `audio` asset；`cover`、`background` 和 `lyrics` 可选且各最多一个；顺序固定为 `audio → cover → background → lyrics`

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
  "maxChunkBytes":36753,
  "window":1,
  "encodings":["base64"],
  "checksums":["crc32","none"],
  "chunkModes":["crc32","none","none-48k"],
  "assets":["audio","cover","background","lyrics"],
  "maxAssets":4,
  "imageFormats":["lvgl-v9-argb8888-bin"]
}
```

旧版快应用未声明 `background` 与 `imageFormats` 时，插件必须省略预处理的 `cover`/`background` BIN，只发送音频和歌词（若有）。新快应用仍接受旧插件发送的 JPG/PNG `cover`，因此升级接收端不会破坏旧插件导入。

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
  "checksum":"crc32",
  "chunkMode":"crc32",
  "assets":[
    {"kind":"audio","size":1234567},
    {"kind":"cover","size":129612,"extension":"bin","format":"lvgl-v9-argb8888-bin","width":180,"height":180},
    {"kind":"background","size":645132,"extension":"bin","format":"lvgl-v9-argb8888-bin","width":336,"height":480},
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
  "maxChunkBytes":35840,
  "checksum":"crc32",
  "chunkMode":"crc32",
  "window":1
}
```

`chunkMode` 为 `crc32`、`none` 或 `none-48k`；后两者省略 `crc32` 字段，`none-48k` 使用 36753 字节原始分块，使 Base64/JSON 序列化后的单包保持在 48 KiB 内，仅适合用户明确接受损坏风险的高速传输。


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

若 Base64 解码或 CRC-32 校验失败，快应用保留当前事务和写入偏移，不写入损坏数据，并请求重传同一个块：

```json
{
  "tag":"lyra-import-retry",
  "id":"transaction-id",
  "asset":"audio",
  "seq":0,
  "offset":0,
  "reason":"checksum-mismatch",
  "attempt":1
}
```

插件只接受与当前窗口中待确认块完全相同的 `asset`、`seq` 和 `offset`，并重发缓存的原始块和 CRC。每块最多重传 2 次；第三次校验失败时快应用终止事务并删除 staging 数据。窗口始终为 1，因此重传不会改变顺序。

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

## 9. 音乐库管理协议 v1

音乐库读取与排序使用独立的 `lyra-library-*` 消息，不改变导入协议 v2。所有消息仍通过 `com.canopus.lyraimport` 路由。导入事务进行中时，快应用以 `busy` 拒绝管理操作。

### 9.1 分页读取

插件发送：

```json
{"tag":"lyra-library-list","version":1,"requestId":"library-1","offset":0,"limit":8}
```

快应用每页最多返回 12 首；插件固定请求 8 首。快应用按 UTF-8 实际字节数把响应动态缩至 7800 字节以内，以确保低于 AstroBox 8192 字节路由上限；插件以下一页的实际 `offset` 连续读取，因此动态缩页不会遗漏曲目。响应不包含物理路径或原始 manifest：

```json
{
  "tag":"lyra-library-page",
  "version":1,
  "requestId":"library-1",
  "revision":"3-a1b2c3d4",
  "offset":0,
  "total":3,
  "tracks":[
    {"id":347230,"name":"歌曲名","artists":["歌手"],"album":"专辑","durationMs":240000}
  ]
}
```

插件必须按连续 `offset` 重建列表，并要求同一次分页读取的 `revision` 保持一致。修订值是由有序曲目 ID 计算的定长不透明字符串；调用方不得解析其内容。

### 9.2 上移或下移

插件基于最近完整读取到的修订值发送：

```json
{
  "tag":"lyra-library-move",
  "version":1,
  "requestId":"library-2",
  "revision":"3-a1b2c3d4",
  "trackId":347230,
  "direction":"up"
}
```

快应用严格解析当前 `library.json`，检查修订值后只交换相邻两项。成功响应：

```json
{"tag":"lyra-library-moved","version":1,"requestId":"library-2","revision":"3-8badf00d","trackId":347230,"from":1,"to":0}
```

插件收到成功响应后必须重新分页读取权威列表，不能只在本地乐观换位。manifest 数组顺序同时是 Lyra 本地列表和上一首/下一首的顺序。

### 9.3 错误

```json
{"tag":"lyra-library-error","requestId":"library-2","code":"conflict"}
```

错误码：

- `busy`：正在导入；
- `invalid-request`：版本、请求 ID、方向或消息格式无效；
- `invalid-library`：现有 manifest 无法严格解析，禁止覆盖；
- `conflict`：读取后曲目顺序已变化，必须刷新；
- `boundary`：曲目已经位于目标方向的边界。

排序发布先写同目录临时文件，并保留旧 manifest 备份；新 manifest 发布失败时恢复旧文件。Vela 文件 API 不提供已验证的覆盖式原子 rename，因此切换期间仍存在极短的无 manifest 窗口，原生播放器应保留上一次成功读取的列表并在后续周期重试。

旧版快应用会把未知管理消息作为 `invalid-request` 返回；插件应提示更新快应用，但不得影响导入协议 v2。使用排序功能时必须同时更新 AstroBox 插件和 Lyra Import 快应用。
