# Card Forge

[English](README.md) | **简体中文**

轻量的 [SillyTavern（酒馆）](https://github.com/SillyTavern/SillyTavern) 角色卡桌面编辑器，用 Rust + [egui](https://github.com/emilk/egui) 编写。

从 JanitorAI / JannyAI 等网站下载的卡，经常把整个 HTML 图库塞在设定字段里。这些图片只是外部链接，而且每次对话都会发给模型。用 Card Forge 可以查看和编辑任意角色卡，把链接里的图片下载到本地缓存，再把它们移出提示词，最后重新写出一张干净的 PNG 卡。

## 功能

- **读取各种格式**：支持 PNG 卡（`chara` / `ccv3` 文本块，也支持 `zTXt` / `iTXt`）和 JSON 卡，V1 / V2 / V3 规范都能读，也认识 Pygmalion 的旧字段名。类型不规范的卡也能读，例如数字版本号、字符串标签、对象形式的世界书条目。
- **编辑所有字段**：名称、作者、版本、标签、描述、性格、场景、开场白、备选开场白、对话示例、系统提示、历史后指令、作者备注，以及内嵌世界书。其余字段可以在原始 JSON 编辑器里改。
- **图片链接**：扫描所有字段里的图片，包括 `<img>` 标签、Markdown 图片、裸链接和 `data:` URI。图片在后台下载到共享缓存，以缩略图浏览，点击可放大，也能设为头像。还可以按卡片中的顺序导出到文件夹或酒馆的角色 Gallery。
- **省 token**：一键把描述、性格等会发给模型的字段里的图片标签移到作者备注。开场白不会被改动，因为那里的图片本来就是要在聊天中显示的。
- **兼容酒馆**：保存时同时写入 `chara`（V2，附带顶层 V1 字段）和 `ccv3`（V3）两个块，做法和 SillyTavern 一致。未识别的字段原样保留。也可以选择移除导出工具留下的私有块。
- **检查文件**：列出 PNG 的所有块，并报告 `IEND` 之后有没有夹带数据。
- **中英双语界面**：默认跟随系统语言，可以在工具栏切换，选择会被记住。

## 构建

需要较新的 Rust 稳定版工具链（edition 2024）。

```bash
git clone https://github.com/numb747/card-forge.git
cd card-forge
cargo build --release
```

编译好的程序在 `target/release/card-forge`。用 `cargo install --path .` 可以安装到 `~/.cargo/bin`。

`cargo install` 之后，执行下面两条命令可以把它加进桌面的应用启动器（桌面会话的 `PATH` 里要包含 `~/.cargo/bin`）：

```bash
install -Dm644 assets/card-forge.desktop ~/.local/share/applications/card-forge.desktop
install -Dm644 assets/icons/card-forge.svg ~/.local/share/icons/hicolor/scalable/apps/card-forge.svg
```

Linux 说明：

- 文件对话框通过 XDG 桌面门户实现，需要安装 `xdg-desktop-portal` 和对应桌面环境的后端。
- 显示中文需要 CJK 字体。装了 Noto Sans CJK 或文泉驿会自动使用，否则会通过 `fc-match` 查找。

## 用法

```bash
card-forge                 # 打开界面
card-forge 卡片.png         # 打开界面并载入卡片
card-forge info 卡片.png    # 在终端打印块列表、字段长度和图片链接
```

- 把 PNG / JSON 卡拖进窗口即可打开，拖入其他图片则替换头像。
- `Ctrl+O` 打开，`Ctrl+S` 保存。
- 图片缓存在 `~/.cache/card-forge/images/`。文件名以链接的哈希开头，多张卡共用缓存，同一张图不会重复下载。
- 「导出到酒馆 Gallery」会写入 `~/SillyTavern/data/default-user/user/images/<角色名>/`。
- 界面语言：在工具栏选择后保存在 `~/.config/card-forge/lang`。设置环境变量 `CARD_FORGE_LANG=zh` 或 `en` 可以覆盖。

## 测试

```bash
cargo test
# 用真实卡片走一遍完整流程（需要联网）：
CARD_FORGE_SAMPLE=卡片.png cargo test -- --ignored
```

## 许可证

[GPL-3.0](LICENSE)
