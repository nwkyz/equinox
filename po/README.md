# 翻译(i18n)

Equinox 的源语言是英文。所有用户可见字符串都在创建处用 `gettext()` 包裹,
**英文不需要任何 .mo 目录**——gettext 找不到翻译时直接回退 msgid(英文)。

当前已有翻译:`po/zh_CN.po`(简体中文)、`po/yue.po`(粤语)、`po/lzh.po`(文言)、`po/ja.po`(日语)、`po/ru.po`(俄语)、`po/bo.po`(藏语,部分词条待补,缺失时回退英文)。

要增加一种新翻译(例如法语):

1. **提取**字符串更新模板(源码字符串有增改时同样执行):

   ```sh
   cargo install xtr
   xtr --package-name equinox --package-version 0.1.0 \
       -o po/equinox.pot $(find crates -name '*.rs' | sort)
   ```

2. **创建**语言目录:

   ```sh
   msginit --locale=fr_FR --input=po/equinox.pot --output=po/fr.po
   # 或复制 .pot 后手工翻译
   ```

3. **翻译** `po/zh_CN.po`(msgstr 列)。

4. **重新编译**:`cargo build --release`。equinox-gui 的 `build.rs` 会把每个
   `po/*.po` 用系统 `msgfmt` 编译到 `target/i18n/<locale>/LC_MESSAGES/equinox.mo`
   (缺 `gettext-tools` 时跳过编译,应用保持英文)。

5. **安装**:`./data/install.sh` 会把 `target/i18n/*` 复制到
   `$PREFIX/share/locale/`。然后用对应 locale 启动:

   ```sh
   LANG=zh_CN.UTF-8 equinox-gui
   ```

## 约定

- **msgid 不带占位符**。带动态内容的消息统一用
  `format!("{}: {e}", gettext("可翻译前缀"))` 的模式——译者只翻译前缀,
  动态尾巴(错误文本、路径、URL、状态码)保持原样。`xtr` 能自动提取这类调用。
- **日志不翻译**(`log::info!`/`warn!` 等)。
- **持久化数据不翻译**(API 返回的壁纸标题、配置键、文件名)。
- `ImageMeta.extra` 的侧边 JSON 键为英文(`summary`、`date`、`description`、
  `license`、`author`)。2026-08 键名英文化之前写入的旧文件带中文键;没有任何
  代码读取它们,故保留不动。
- 桌面文件保留 `[zh_CN]` 变体(这是标准做法)。

## locale 目录解析顺序

`equinox_core::i18n::init()` 绑定第一个存在的目录:
`$APPDIR/usr/share/locale`(AppImage)→ `$XDG_DATA_HOME/locale` →
`$HOME/.local/share/locale` → `/usr/share/locale`(deb)。
未来做 flatpak 时,应在打包时通过构建期 env 把 `/app/share/locale` 放到最前。
