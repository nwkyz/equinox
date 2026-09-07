# Equinox 翻译词条列表

> 共 **112** 条。英文为 msgid(源语言),「简体参考」列为现有 zh_CN.po 的译法,仅供理解语义。
>
> 翻译方法:在 `po/` 下新建 `<语言代码>.po`(如 `bo.po` 藏语),把译文填入对应条目的 `msgstr ""`,然后 `cargo build` 即自动编译为 .mo 并生效。骨架文件 `po/bo.po` 已就绪,直接填空即可。

## 注意事项

- **#70 `" · Success"`**(前导有空格+间隔号)与 **#48 `(catch-up)`** 是拼接片段,译文需保持可拼接性
- **#24 `Released under the`** 后面固定拼接 "GNU GPL v3 or later"(不译),中文译法是「许可证:」这类改写,其他语言可用等效结构
- **#104 `Equinox`** 为应用名,建议保留原文
- 占位词如 `org.gnome.desktop.background`、`schema`、`hdurl`、`APOD`、URL 等技术名词不译

## 词条表

| # | English (msgid) | 简体参考 |
|---|---|---|
| 1 | Missed update window (powered off) | 错过更新时机(关机期间) |
| 2 | Updating | 更新 |
| 3 | Unknown source | 未知来源 |
| 4 | This source has no images yet, run "Update now" first | 该来源还没有图片,请先运行“立即更新” |
| 5 | No wallpaper source selected yet, choose one on the Wallpaper page | 尚未选择壁纸来源,请在“壁纸”页面选择一个 |
| 6 | Setting wallpaper | 正在设置壁纸 |
| 7 | Applying image | 正在应用图像 |
| 8 | Applying previous | 正在应用上一张 |
| 9 | No earlier wallpaper in history | 历史记录中没有更早的壁纸 |
| 10 | File does not exist | 文件不存在 |
| 11 | Failed to apply wallpaper | 应用壁纸失败 |
| 12 | The daemon is not running; scheduled updates will be unavailable | 守护进程未运行,定时更新将不可用 |
| 13 | Start | 启动 |
| 14 | Wallpaper | 壁纸 |
| 15 | Sources | 来源 |
| 16 | Gallery | 图库 |
| 17 | History | 历史 |
| 18 | Main menu | 主菜单 |
| 19 | Tasks | 任务 |
| 20 | Timeline | 时间线 |
| 21 | Preferences | 偏好设置 |
| 22 | About | 关于 |
| 23 | Multi-source daily wallpaper manager | 多来源每日壁纸管理器 |
| 24 | Released under the | 许可证: |
| 25 | No running tasks | 暂无任务 |
| 26 | Auto (follow system) | 自动(跟随系统) |
| 27 | Language | 语言 |
| 28 | Restart Equinox to apply the new language | 重启 Equinox 后新语言生效 |
| 29 | Daemon is not running | 守护进程未运行 |
| 30 | Refresh | 刷新 |
| 31 | Clear history | 清空历史 |
| 32 | Applied | 应用于 |
| 33 | No wallpaper has been applied yet | 尚未应用过壁纸 |
| 34 | Wallpapers you apply will be recorded here so you can go back to them | 应用的壁纸会记录在这里,可随时切换回去 |
| 35 | entries | 条 |
| 36 | History cleared | 已清空历史 |
| 37 | Failed to clear history | 清空历史失败 |
| 38 | Wallpaper sources | 壁纸来源 |
| 39 | Enable scheduled updates | 启用定时更新 |
| 40 | Refresh interval (seconds) | 刷新间隔(秒) |
| 41 | Max images (0 = unlimited) | 图片数量上限(0 = 不限制) |
| 42 | Status | 状态 |
| 43 | Update now | 立即更新 |
| 44 | Fetch one image immediately and store it in the image directory | 立即抓取一张图像并保存到图像目录 |
| 45 | Update | 更新 |
| 46 | Updating… | 更新中… |
| 47 | Update failed | 更新失败 |
| 48 | images | 张 |
| 49 | Previous image | 上一张 |
| 50 | Next image | 下一张 |
| 51 | Loading… | 加载中… |
| 52 | Previous source | 上一个来源 |
| 53 | Next source | 下一个来源 |
| 54 | No images in this source yet | 此来源还没有图片 |
| 55 | Switch sources below or run an update first | 请在下方切换来源,或先执行一次更新 |
| 56 | Failed to switch | 切换失败 |
| 57 | Failed to set | 设置失败 |
| 58 | Operation failed | 操作失败 |
| 59 | Random | 随机一张 |
| 60 | Manual | 手动 |
| 61 | Latest | 最新一张 |
| 62 | Untitled wallpaper | 未命名壁纸 |
| 63 | Carousel | 轮播 |
| 64 | Grid | 网格 |
| 65 | Delete selected | 删除所选 |
| 66 | No images yet | 还没有图片 |
| 67 | Go to the Sources page and click "Update now" to download some wallpapers | 前往“来源”页面点击“立即更新”,下载一些壁纸 |
| 68 | Image deleted | 已删除图像 |
| 69 | Failed to delete | 删除失败 |
| 70 | sec | 秒 |
| 71 | min | 分 |
| 72 | h | 小时 |
| 73 | d | 天 |
| 74 | Scheduled | 定时 |
| 75 | Apply | 应用 |
| 76 | Each source updates on its own interval. Windows missed while the computer was off or asleep are caught up right after boot or wake-up. | 每个来源按各自设置的间隔独立更新。计算机关机或睡眠期间错过的更新,会在开机或唤醒后立即补跑。 |
| 77 | Planned | 计划 |
| 78 | Next | 下次 |
| 79 | Every | 每 |
| 80 | Scheduled updates are off | 定时更新已关闭 |
| 81 | (catch-up) | (补跑) |
| 82 | " · Success" | " · 成功" |
| 83 | No tasks recorded yet | 暂无任务记录 |
| 84 | this environment cannot set the GNOME wallpaper (missing org.gnome.desktop.background schema) | 当前环境无法设置 GNOME 壁纸(缺少 org.gnome.desktop.background schema) |
| 85 | Invalid URL | 无效的 URL |
| 86 | Network request failed | 网络请求失败 |
| 87 | NASA | NASA |
| 88 | API key (optional) | API 密钥(可选) |
| 89 | Use high-definition image (hdurl) | 使用高清图像(hdurl) |
| 90 | Today's APOD entry is a video, skipped | 今日 APOD 条目是视频,已跳过 |
| 91 | Wikimedia | Wikimedia |
| 92 | Thumbnail width | 缩略图宽度 |
| 93 | Bing | 必应 |
| 94 | Region | 区域 |
| 95 | Resolution | 分辨率 |
| 96 | Include history | 包含历史图片 |
| 97 | Auto | 自动 |
| 98 | Windows Spotlight | Windows 聚焦 |
| 99 | Country | 国家 |
| 100 | Locale | 语言区域 |
| 101 | Spotlight | 聚焦 |
| 102 | Failed to create directory | 创建目录失败 |
| 103 | Failed to write image | 写入图像失败 |
| 104 | Failed to write description | 写入描述失败 |
| 105 | Apply wallpaper | 应用为壁纸 |
| 106 | Set as wallpaper | 设为壁纸 |
| 107 | Wallpaper applied | 壁纸已应用 |
| 108 | Apply the selected image as wallpaper | 将所选图片应用为壁纸 |
| 109 | Fill mode | 填充模式 |
| 110 | Maximize | 最大化 |
| 111 | Close | 关闭 |
| 112 | Fit mode | 适应模式 |

## 界面语言选择器中的显示名(app.rs 内,非 gettext 词条)

| 值 | 显示名 |
|---|---|
| `auto` | Auto (follow system)(gettext 词条) |
| `en` | English |
| `zh_CN` | 中文 (普通话) |
| `yue` | 中文 (粤语) |
| `lzh` | 中文 (文言) |
| `ja` | 日本語 |
| `ru` | Русский |
| `bo` | བོད་ཡིག |
