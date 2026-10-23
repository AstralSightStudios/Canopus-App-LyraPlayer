# Lyra Connect 协议 v1

AstroBox 插件 `astrobox-lyra-import` 与手表上的 Lyra Player 模块（`lyra_player`）通过 CanopusConnect
（ADR-CAN-009）交换二进制帧：音乐文件以数据流直接写进模块目录，不经过快应用、不经过 interconnect。
协议的编解码在 `crates/lyra-protocol`，手机侧发送状态机在 `crates/lyra-transfer`，手表侧接收状态机在
`crates/lyra-player-core/src/import.rs`；三者共用同一份定义，`tools/lyra-mirage-import` 用同一个发送器在 Mirage 里端到端验证。

## 1. 架构

```text
本地 MP3 / 网易云音乐
        │
        ▼
AstroBox 插件（canopusconnect.send / listen）
        │  CanopusConnect 消息，每条最多 480 字节，有序、不重复
        ▼
Canopus Supervisor ── ctx->connect ──▶ lyra_player（connect.rs → Importer）
                                          │
                                          ▼
                      /data/canopus/m.lyra_player/{library.json, tracks/<id>/…}
```

* 插件权限：`canopusconnect.send`、`canopusconnect.listen`、`device`。插件在手表连接后 `listen` 模块
  `lyra_player`，之后收到的手表消息通过 `on-message` 回调送达。
* 固件要求：需要 `ctx->connect`（CONNECT_SEND / CONNECT_LISTEN / CONNECT_MESSAGE）。Band 11 `.155`、`.179` 与 Band 10 Pro
  `.043` 已在 Mirage 验证；按 ADR-CAN-009 只有带 L2 后端的目标提供它，`.036`、`.139` 没有（未验证）。没有时播放器照常运行，
  曲库页空状态会提示该固件不能从手机导入。
* 没有手表端 UI 依赖：导入在后台完成，播放器页面不需要打开。

## 2. 帧格式

全部整数小端。第一个字节是操作码。

| 方向 | 格式 |
|---|---|
| 手机 → 手表请求 | `[op][req: u16][payload]`，`DATA` 例外：`[0x11][asset: u8][offset: u32][bytes]`，没有自己的应答 |
| 手表 → 手机应答 | `[op \| 0x80][req: u16][status: u8][payload]` |
| 手表 → 手机事件 | `[0xC0 + n][payload]` |

字符串为 `[len: u8][UTF-8]`，超长时在字符边界截断。资源类型：`1` 音频（MP3）、`2` 封面、`3` 歌词（LRC 文本）。

### 请求

| op | 名称 | payload |
|---|---|---|
| `0x01` | `HELLO` | `version: u8`（当前 1） |
| `0x02` | `LIST` | `offset: u16, max: u8` |
| `0x10` | `BEGIN` | `id: u64, duration_ms: u32, palette_top: u32, palette_bottom: u32, name, artist_count: u8, artist…, album, spec_count: u8, {kind: u8, size: u32, crc32: u32}…` |
| `0x11` | `DATA` | 见上，最多 464 字节数据 |
| `0x12` | `ASSET_END` | `asset: u8` |
| `0x13` | `COMMIT` | `id: u64` |
| `0x14` | `ABORT` | — |
| `0x20` | `DELETE` | `id: u64` |
| `0x21` | `MOVE` | `id: u64, up: u8` |
| `0x30` | `PLAY` | `id: u64` |
| `0x31` | `CONTROL` | `command: u8`（1 播放/暂停，2 下一首，3 上一首） |

### 应答

* `HELLO`：`status, version: u8, max_data: u16, free_kib: u32, tracks: u16, revision: u32, width: u16, height: u16`。
  `width/height` 是手表屏幕分辨率（Band 11 为 212×520，Band 10 Pro 为 336×480），插件据此生成封面。
  `free_kib = 0xFFFFFFFF` 表示固件没有给出剩余空间。
* `LIST`：`status, total: u16, offset: u16, count: u8, {id: u64, duration_ms: u32, name, artist}…`
  单帧装不下时 `count` 小于请求的 `max`，插件继续翻页。
* 其他请求：`status, value: u32`；`COMMIT`、`DELETE`、`MOVE` 的 `value` 是之后的曲库修订号。

状态码：`0 Ok`、`1 BadRequest`、`2 NoSpace`、`3 Busy`、`4 Io`、`5 Crc`、`6 NotFound`、`7 Unsupported`、
`8 Exists`（文件系统报告目标已存在）、`9 Sequence`。

### 事件

* `0xC0 PROGRESS`：`asset: u8, next_offset: u32, status: u8`——手表已经安全写入 `asset` 的前 `next_offset` 字节。
* `0xC1 NOW_PLAYING`：`id: u64, state: u8, position_ms: u32, duration_ms: u32, volume: u8`，播放状态变化时推送。

## 3. 导入一首歌

```text
phone                                      watch
  │ BEGIN(meta, specs[audio,cover,lyrics]) ─▶│  检查空间与上限，清空并重建 stage/
  │◀──────────────────────────── Ack(Ok)    │
  │ DATA(asset, offset, bytes) ──────────── ▶│  顺序写入，逐块累计 CRC32
  │ DATA … （窗口 16 KiB）                   │
  │◀──────────────── PROGRESS(asset, next)  │  至少每 4 KiB 一次
  │ ASSET_END(asset) ─────────────────────▶ │  核对大小与 CRC32
  │◀──────────────────────────── Ack(Ok)    │
  │ COMMIT(id) ───────────────────────────▶ │  stage → tracks/<id>，替换 library.json
  │◀────────────────────── Ack(Ok, revision)│
```

* 一次只有一个事务；新的 `BEGIN` 会丢弃未完成的旧事务，`ABORT` 显式取消。
* **流控与续传**：手机最多有 16 KiB 未确认数据在途。手表看到不是期望的偏移时不丢弃连接，
  而是发 `PROGRESS(next_offset = 它期望的偏移, status = Sequence)`，手机回退到该偏移重发。
  手机侧对同一偏移的重复回退做了去抖（`rewound_to`）。
* `BEGIN` 必须带且只带一份音频资源，封面与歌词可选，每份大小大于 0 且不超过上限（音频 64 MiB、封面 256 KiB、歌词 64 KiB），否则回 `BadRequest`。
  `id` 已存在时 `COMMIT` 整体替换旧目录；替换不需要额外空间。固件没有给出剩余空间（`free_kib = 0xFFFFFFFF`）时不做空间预检。
* 手表 60 秒没有收到手机的帧就丢弃事务；插件 45 秒没有收到进度就取消传输。
* **吞吐是往返时延决定的**：CPC2 每条消息一问一答，AstroBox 侧每台设备同时只有一条在途（ADR-CAN-009），CPC2 载荷上限
  512 字节、CONNECT 数据上限 480 字节。Mirage（043）里实测：一条 `CONNECT_SEND` 的往返中位数约 35 ms，`DATA` 帧 464 字节，
  所以约 11.7 KiB/s；帧大小 8 字节与 464 字节耗时相同，并发 1/2/4/8 条在途也没有变化（`lyra-mirage-import bench` /
  `import --pipeline N`）。发送端已经贴着这个下限（每帧 38.6 ms 对 35 ms 往返）。要更快只能改框架：更大的 CONNECT 帧
  （需要放宽 CPC2 的 512 字节载荷与 Supervisor 的静态缓冲）或允许多条在途。真机往返时延尚未测量。

## 4. 手表上的存储

```text
/data/canopus/m.lyra_player/
  library.json                  { "version": 2, "revision": N, "tracks": [Song…] }
  tracks/<id>/audio.mp3         音频
  tracks/<id>/cover.bin         176×176 LVGL v9 ARGB8888，圆角透明，12 字节头
  tracks/<id>/lyrics.lrc        UTF-8 LRC（可含翻译行）
  stage/                        未提交的事务（同一时间只有一个）
  glyph-*.bin, appicon.bin      模块首次运行时自己写出的图标位图
```

`Song` 字段：`id, name, artists[], album, duration_ms, assets（位掩码）, palette{top, bottom}`。
`palette` 是从封面取的两种颜色，播放页背景在它们之间渐变；曲库修订号 `revision` 每次 `COMMIT`/`DELETE`/`MOVE` 加一，
插件用它发现手表上的曲库已被改动。

## 5. 测试

* `cargo test --workspace` 覆盖编解码、手机端发送状态机、手表端接收状态机（含乱序、重复、CRC 错误、空间不足）。
* `scripts/mirage_session.py <155|043> cycle --seed 4` 安装当前构建并重灌示例曲库；
  `python3 - <<'EOF'`（见 `scripts/mirage_session.py` 的 `Session.lyra`）调用 `tools/lyra-mirage-import`，
  通过 AstroBox 桥发送 `hello` / `list` / `import` 命令，与插件使用同一份 `lyra-transfer` 发送器。
