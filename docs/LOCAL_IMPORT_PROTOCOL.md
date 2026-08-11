# Lyra 本地 MP3 导入协议（预留接口 v1）

本文定义手机端工具通过 Canopus 原生 `interconnect` 通道向 Lyra 导入 MP3 的接口。它与 FetchBridge v4 共用同一条 UTF-8 JSON 传输，但使用独立的 `lyra-import-*` 标签；FetchBridge 消息仍按其原协议处理。

> 当前播放器已经能够读取持久化曲库并播放 `Song.local_path` 指向的文件。本协议是手机端导入器与手表端接收器的稳定预留接口，后续实现不得改变 v1 字段语义。

## 1. 存储位置

- 音频文件：`/data/canopus/<安全文件名>.mp3`
- 曲库索引：`/data/canopus/lyra-player-library.json`
- 临时文件：`/data/canopus/.lyra-import-<id>.tmp`
- 最大文件名：96 个 UTF-8 字节。
- 文件名不得为空、以 `.` 开头、包含 `/`、`\`、NUL、控制字符或 `..` 路径片段，且扩展名必须是小写 `.mp3`。

导入必须先写临时文件，校验长度和 SHA-256 后再原子重命名。失败或取消时删除临时文件，不得覆盖现有曲库记录。

## 2. 传输约束

- 每条 interconnect 消息是一段 UTF-8 JSON 对象。
- 二进制分片使用 RFC 4648 标准 Base64。
- `maxChunkBytes` 默认 2048，指 Base64 编码前字节数。
- v1 使用单分片窗口：发送下一片前必须收到累计 ACK，避免填满 BLE/QAIC 队列。
- `seq` 从 0 开始；`offset` 是原始 MP3 字节偏移。
- `crc32` 是原始分片的 IEEE CRC-32，小写 8 位十六进制，与 FetchBridge v4 算法一致。
- `id` 在一次连接内唯一，建议使用 16～32 位随机十六进制字符串。
- 所有整数必须能无损表示为 JSON 非负整数。

## 3. 能力握手

手机端发送：

```json
{
  "tag": "lyra-import-hello",
  "version": 1,
  "maxChunkBytes": 2048,
  "encodings": ["base64"],
  "sha256": true
}
```

手表端回复：

```json
{
  "tag": "lyra-import-hello",
  "version": 1,
  "maxChunkBytes": 2048,
  "window": 1,
  "freeBytes": 16777216
}
```

若版本没有交集，接收方回复 `lyra-import-error`，`code` 为 `unsupported-version`。

## 4. 导入流程

### 4.1 开始

```json
{
  "tag": "lyra-import-begin",
  "id": "a13f09c2",
  "fileName": "Orbit.mp3",
  "size": 4831021,
  "sha256": "9c2b...64个小写十六进制字符...",
  "track": {
    "id": 0,
    "name": "Orbit",
    "artists": ["Local Artist"],
    "album": "Local",
    "durationMs": 213000
  }
}
```

`id: 0` 表示本地曲目。手表端验证参数、剩余空间及重名策略后回复：

```json
{
  "tag": "lyra-import-ready",
  "id": "a13f09c2",
  "nextSeq": 0,
  "nextOffset": 0,
  "maxChunkBytes": 2048,
  "window": 1
}
```

v1 不支持断点续传；`nextSeq` 和 `nextOffset` 固定从 0 开始。

### 4.2 数据分片

```json
{
  "tag": "lyra-import-chunk",
  "id": "a13f09c2",
  "seq": 0,
  "offset": 0,
  "encoding": "base64",
  "data": "SUQzBAAAAA...",
  "crc32": "34a921f0"
}
```

成功落盘后回复累计 ACK：

```json
{
  "tag": "lyra-import-ack",
  "id": "a13f09c2",
  "nextSeq": 1,
  "nextOffset": 2048
}
```

重复收到已经 ACK 的分片时，接收方不得重复写入，应重新发送当前累计 ACK。序号、偏移或 CRC 不符时发送错误并终止该导入。

### 4.3 提交

所有字节均已 ACK 后，手机端发送：

```json
{
  "tag": "lyra-import-commit",
  "id": "a13f09c2",
  "size": 4831021,
  "sha256": "9c2b..."
}
```

手表端按以下顺序处理：

1. 关闭并同步临时文件；
2. 校验实际长度；
3. 校验整个文件 SHA-256；
4. 原子重命名为最终 `.mp3` 路径；
5. 原子更新 `lyra-player-library.json`；
6. 刷新 Lyra 本地曲库模型；
7. 回复完成消息。

```json
{
  "tag": "lyra-import-done",
  "id": "a13f09c2",
  "path": "/data/canopus/Orbit.mp3"
}
```

## 5. 取消和错误

任意一方可发送：

```json
{
  "tag": "lyra-import-cancel",
  "id": "a13f09c2",
  "reason": "user-cancelled"
}
```

错误格式：

```json
{
  "tag": "lyra-import-error",
  "id": "a13f09c2",
  "code": "checksum-mismatch",
  "message": "chunk crc32 mismatch"
}
```

稳定错误码：

- `unsupported-version`
- `invalid-request`
- `invalid-name`
- `already-exists`
- `no-space`
- `out-of-order`
- `checksum-mismatch`
- `length-mismatch`
- `io-error`
- `busy`
- `cancelled`

错误或连接中断后，手表端必须关闭文件并删除对应临时文件。一次只允许一个活动导入；其它 `begin` 返回 `busy`。

## 6. 曲库查询与删除（可选扩展）

查询：

```json
{"tag":"lyra-import-list","requestId":"list-1"}
```

响应可以按 20 项分页：

```json
{
  "tag": "lyra-import-list-result",
  "requestId": "list-1",
  "offset": 0,
  "total": 1,
  "tracks": [
    {
      "name": "Orbit",
      "artists": ["Local Artist"],
      "durationMs": 213000,
      "fileName": "Orbit.mp3",
      "size": 4831021
    }
  ]
}
```

删除必须由用户在手表端确认后才能执行：

```json
{"tag":"lyra-import-delete","requestId":"del-1","fileName":"Orbit.mp3"}
```

## 7. 安全要求

- 不信任手机端提供的路径，只接受文件名并由手表端拼接固定目录。
- 写入前后都执行文件名验证，禁止路径穿越和符号链接目标。
- 对 JSON 帧、Base64 解码结果、声明大小和累计接收大小设置硬上限。
- 未完成 SHA-256 校验前不得把曲目写入曲库索引。
- 日志和错误消息不得包含网易云 cookie、会话 JSON 或 MP3 内容。
- FetchBridge、播放流和本地导入共享 interconnect 时，接收方按 `tag` 分发；未知标签只记录并忽略，不得断开连接。
