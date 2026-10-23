# Lyra Player

Canopus 原生音乐播放器模块，加上把音乐直接送进手表的 AstroBox 插件。构建时需要同级的 `Canopus`
框架仓库，也可通过 `CANOPUS_ROOT` 指定框架目录。

| 目标 | 播放器 | 导入（CanopusConnect） |
|---|---|---|
| Band 11 `.155` | ✔ Mirage 验证 | ✔ Mirage 验证 |
| Band 11 `.179` | ✔ Mirage 验证 | ✔ Mirage 验证（`HELLO`） |
| Band 11 `.139` | 构建与验证器通过 | 按 ADR-CAN-009 没有 `ctx->connect`，不能导入（未验证） |
| Band 10 Pro `.043` | ✔ Mirage 验证 | ✔ Mirage 验证 |
| Band 10 Pro `.036` | 构建与验证器通过 | 按 ADR-CAN-009 没有 `ctx->connect`，不能导入（未验证） |

真机上的音频输出、触摸手感与传输速率尚未验证；以上“Mirage 验证”只是模拟器证据。

模块按 Canopus 公共 ABI 2.0 构建（`canopus_abi = "2"`，随框架的 UI V2 一起升级）：设备上必须先装上同一版框架的
Supervisor（用框架的安装器表盘重新 Run 一次），旧版（ABI 1）Supervisor 不会加载这个模块。

## 组成

* `crates/lyra-player-core`：与设备无关的播放器——曲库、LRC 歌词、播放状态机、两种屏幕的布局
  （`layout.rs`）、页面视图模型（`view.rs`）、手表端导入状态机（`import.rs`），全部可在主机上测试。
* `crates/lyra-player-device`：Canopus 目标绑定——音频、存储、CanopusConnect 端点（`connect.rs`）、原生应用注册，
  以及 LVGL 界面（`target/ui/`：曲库页、播放页、歌词页）。图标位图由 `scripts/build-glyphs.py` 从
  `assets/glyphs/*.svg` 生成，随模块嵌入，首次运行时写进模块目录。
* 应用图标是 `crates/lyra-player-device/assets/appicon.bin`（117×117 的 LVGL ARGB8888，手绘），随模块嵌入，
  模块注册时写进自己的目录，内容不同就覆盖；其中 alpha 为 0x0c 的像素会被验证器当成固件地址，`Canopus.toml`
  的 `allowed_constants` 里列着它们，设备 crate 里有测试保证列表完整，换图标时跟着更新。
* `crates/lyra-protocol`、`crates/lyra-transfer`：导入协议的编解码与手机端发送状态机，见
  [docs/CONNECT_PROTOCOL.md](docs/CONNECT_PROTOCOL.md)。
* `plugins/astrobox-lyra-import`：AstroBox 插件，本地 MP3 / 网易云音乐 → 封面与歌词 → CanopusConnect 二进制流。
  构建见 [插件 README](plugins/astrobox-lyra-import/README.md)。
* `tools/lyra-mirage-import`：在 Mirage 里用同一份发送器驱动导入，供端到端验证。

## 界面

* 主页：“全部歌曲”加上你在插件里建的歌单，每行显示名称和歌曲数；歌单是可选的，不建也不影响（主页就只有“全部歌曲”一行）。
  点一行进入该集合的歌曲页（侧滑返回），页首的“随机播放”“循环播放”会清空播放队列、把这个集合放进去再开始。
  一首歌可以在多个歌单里，也可以不在任何歌单里；歌单在 AstroBox 插件里新建、重命名、增删歌曲（需要手表模块协议版本 2）。
* 歌曲页：纯黑背景，大标题随列表一起滚走；每行是封面缩略图、歌名、艺人和内缩的分隔线，按下时整行高亮，
  末尾显示歌曲总数。正在播放的那一行歌名换成强调色，封面上叠一组律动条（暂停时静止）。
  有歌曲在播放时，底部浮着一条迷你播放条：缩略图、滚动歌名（Band 10 Pro 再加一行艺人），右侧是播放/暂停键，
  点条身进入播放页。
* 播放模式有四种，点播放页右上角的图标循环切换：列表播放、列表循环、随机播放、单曲循环。播放队列就是当前集合
  （全部歌曲或某个歌单），四种模式都只在队列里转；单曲循环只在自然播完时重播本曲，手动上一首/下一首照常切歌。
* 播放页：封面带柔和的投影，下面是歌名、艺人、进度条和时间，然后是不带底板的上一首/播放/下一首图标
  （按下时才出现一圈高亮），Band 11 底部是 −/音量条/+，Band 10 Pro 的音量键在控制行两侧。背景是从封面取色的纵向渐变。
* 歌词页与播放页在同一个横向分页里，左右滑动切换；歌词自动跟随播放进度，手指拖动时暂停跟随、松手几秒后回到当前行，
  当前行放大高亮，其余行渐隐。
* 动画：换行时新的当前行以轻微回弹（ease-out-back）放大、其余行缩小变暗，列表以 ease-in-out 滑到新行；
  切歌时封面、歌名、艺人朝切换方向滑出淡出、新歌从另一侧滑入（下一首从右、上一首从左），背景色同时渐变；
  曲库页的迷你播放条在后台切歌时内容从右侧淡入。固件的 `lv_anim` 模块够不到，所以由页面在有东西在动时才开一个
  16 ms 的 LVGL 定时器，数值全按经过的时间计算（`lyra-player-core` 的 `motion.rs` 是定点缓动曲线）。

列表缩略图不经手机传输：手表第一次需要时从 `cover.bin` 面积平均缩小、烘焙圆角，存为同目录的 `thumb.bin`
（`lyra-player-core` 的 `thumbnail.rs`，每个刷新 tick 最多生成一张；重新导入会删掉旧缩略图）。
两种分辨率（Band 11 212×520、Band 10 Pro 336×480）共用同一套视图逻辑，差别只在 `layout.rs` 的度量表与
`ui/lv.rs` 里的固件细节。

## 截图与性能

* 模拟器截图见 [docs/screenshots](docs/screenshots/README.md)（由 `scripts/mirage_screenshots.py` 生成）；
* 界面耗时与传输速度的实测、方法和结论见 [docs/PERFORMANCE.md](docs/PERFORMANCE.md)。

## 构建与打包

```sh
canopus build                 # 全部目标：交叉编译并运行 ELF 验证器
canopus package --output dist/lyra_player.canopus
cargo test --workspace        # 主机测试
(cd plugins/astrobox-lyra-import && python3 scripts/build_dist.py --release --package)   # 插件 .abp
```

## 在 Mirage 里开发

需要 Mirage、对应固件、Facetory 打包器（`FACETORY_PACKAGER`）和 AstroBox 桥（`MAB_SMOKE`），
详见各脚本文件头。

```sh
python3 scripts/mirage_session.py 155 create            # 新建设备、配对、装生产安装表盘（043 同理）
python3 scripts/mirage_session.py 155 deploy --seed 4   # 任意状态下：开机 → Run → 打包并 CPC2 安装当前构建 → 重启 → Run，重灌 4 首示例曲库
python3 scripts/mirage_session.py 155 cycle --seed 4    # 同上，但设备已经开机并 Run 过时更快
python3 scripts/mirage_ui_test.py 155 library player lyrics back --shots shots   # 截图场景
python3 scripts/mirage_record.py 155 rec lyrics swap library                      # 录屏（连续截图拼成 mp4）检查动画
```

* 新安装的模块默认禁用：`mirage_session.py <155|179|043> enable` 发 `canopusSetEnabled` 启用，重启后生效；`restart` 之后启动器发布图标。
  Band 10 Pro 须先 Run 过（Supervisor 在内存里）才能安装模块更新，否则 CPC2 超时、旧模块照跑。
* 模拟器没有蓝牙音频 sink，示例曲库带 `dry-run` 标记，播放器照常计时但不输出音频。
* `scripts/seed_library.py`、`scripts/mirage_dev.py`（唤醒、点击、滑动、启动器导航）可单独使用。

## Prod 安装表盘

```sh
scripts/build-install-watchface-prod.sh xiaomi-band-11
```

不指定设备时分别构建 10 Pro（`.036`、`.043` 同目录）与 Band 11（`.139`、`.155` 同目录）安装包。
也可传入一个或多个完整 target ID，或设置 `CANOPUS_DEVICE` / `CANOPUS_TARGET`；
脚本参数优先，其次 `CANOPUS_DEVICE`。选择单个版本会重建对应设备包，保留其他设备目录。
脚本会交叉编译、运行 ELF verifier、签名 CMI1 凭证，并从框架共享模板生成 prod Lua。
它复用 BluetoothAudio 的安装协议以及框架的 UI 进度组件。
Band 11 安装 Lua 仅用普通 `io.open` 访问 `/canopus/install`，不包含 execute 恢复或 debug 操作。
10 Pro 安装 Lua 沿用改造前的流程（`/dev/canopus`、`getprop` 回退、`mkdir` 准备 inbox），
按 `ro.build.customer_version` 核对精确构建号（10 Pro 的 `ro.build.id` 不含日期后缀）。
每次写入及回读、校验和安装提交都有 UI 状态提示。

产物目录为 `watchfaces/lyra-player-prod/xiaomi-band-10-pro/` 与
`watchfaces/lyra-player-prod/xiaomi-band-11/`，各设备目录根部仅有一个 `main.lua` 与 `.bin` 资源。
将对应设备目录交给表盘打包器；子目录 `build/` 与 `docs/` 不参与打包。
Band 11 须先更新 Canopus 框架安装表盘；旧 Supervisor 已常驻时须重启后运行新版，
由框架准备 `/data/canopus/inbox` 和新安装端点。10 Pro 不依赖新端点。安装后的模块保持禁用。
`LYRA_PLAYER_ICON` 可以指定应用图标 PNG；私钥不会进入安装包。
