# 截图

Mirage 模拟器里的截图，不是真机。由 `scripts/mirage_screenshots.py KEY docs/screenshots` 在已部署当前构建的设备上生成
（`mirage_session.py KEY deploy --seed 4` 之后）；`KEY` 是 `155`、`179`（Band 11，212×520）或 `043`（Band 10 Pro，336×480）。
曲库是脚本灌入的 4 首示例歌（颜色是合成的封面）。

![overview](overview.png)

上排 Band 11 `.155`，下排 Band 10 Pro `.043`：曲库、播放页、歌词页、带“正在播放”卡片的曲库。

| 文件 | 内容 |
|---|---|
| `<KEY>-launcher.png` | 启动器里的 Lyra 图标（Band 10 Pro 的图标在末行首格，被系统启动器裁掉一部分） |
| `<KEY>-library.png` | 曲库页 |
| `<KEY>-player.png` | 播放页 |
| `<KEY>-volume.png` | 点音量键后的音量显示（Band 10 Pro 临时替换标题行） |
| `<KEY>-lyrics.png` | 歌词页，自动跟随，当前行高亮 |
| `<KEY>-lyrics-browse.png` | 手指拖动歌词后的浏览状态 |
| `<KEY>-library-playing.png` | 返回曲库，顶部是“正在播放”卡片 |

空曲库的提示页没有放进来：脚本的 `--empty` 选项可以生成，但这批截图之后又改了提示文案，没有重拍。
