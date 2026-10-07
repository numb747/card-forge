# Card Forge

**English** | [简体中文](README.zh-CN.md)

A lightweight desktop editor for [SillyTavern](https://github.com/SillyTavern/SillyTavern) character cards, written in Rust with [egui](https://github.com/emilk/egui).

Cards downloaded from sites like JanitorAI / JannyAI often carry a whole HTML image gallery inside their definition fields. Those images are only remote links, and they get sent to the model on every message. Card Forge lets you inspect and edit any card, download the linked images to a local cache, move them out of the prompt, and write a clean PNG card back.

## Features

- **Reads every common format**: PNG cards (`chara` / `ccv3` text chunks, also `zTXt` / `iTXt`) and JSON cards, in spec V1, V2 and V3, plus legacy Pygmalion field names. Badly typed cards are accepted too (numeric versions, string tags, object-shaped lorebook entries, …).
- **Edits all fields**: name, creator, version, tags, description, personality, scenario, first message, alternate greetings, example dialogue, system prompt, post-history instructions, creator notes, and the embedded lorebook. A raw JSON editor covers everything else.
- **Image links**: finds images in every field (`<img>` tags, Markdown images, bare links, `data:` URIs), downloads them in the background into a shared cache, shows a thumbnail gallery with click-to-zoom, and can set any image as the avatar. You can also export the images in card order to a folder or to SillyTavern's per-character gallery.
- **Token cleanup**: one click moves image tags out of model-visible fields (description, personality, …) into the creator notes. Greetings are left alone, because images there are meant to show up in the chat.
- **SillyTavern-compatible output**: saving writes both a `chara` chunk (V2, with top-level V1 mirror fields) and a `ccv3` chunk (V3), the same as SillyTavern. Unknown fields are preserved verbatim. Optionally strips private chunks left behind by export tools.
- **Inspection**: lists every PNG chunk and reports any data hidden after `IEND`.
- **Chinese and English UI**: follows the system locale, can be switched from the toolbar, and remembers your choice.

## Build

Requires a recent stable Rust toolchain (edition 2024).

```bash
git clone https://github.com/numb747/card-forge.git
cd card-forge
cargo build --release
```

The binary ends up at `target/release/card-forge`. Use `cargo install --path .` to install it into `~/.cargo/bin`.

Linux notes:

- File dialogs go through the XDG desktop portal (`xdg-desktop-portal` plus a backend for your desktop).
- Chinese text needs a CJK font. Noto Sans CJK or WenQuanYi is picked up automatically; otherwise `fc-match` is used to find one.

## Usage

```bash
card-forge                 # open the GUI
card-forge card.png        # open the GUI with a card loaded
card-forge info card.png   # print chunks, field sizes and image links in the terminal
```

- Drop a PNG / JSON card onto the window to open it. Drop any other image to use it as the avatar.
- `Ctrl+O` opens a card and `Ctrl+S` saves it.
- Image cache: `~/.cache/card-forge/images/`. File names start with a hash of the URL, so the cache is shared between cards and nothing is downloaded twice.
- "Export to SillyTavern gallery" writes to `~/SillyTavern/data/default-user/user/images/<card name>/`.
- UI language: chosen in the toolbar and saved to `~/.config/card-forge/lang`. Set `CARD_FORGE_LANG=zh` or `en` to override it.

## Tests

```bash
cargo test
# Full round trip on a real card (needs network access):
CARD_FORGE_SAMPLE=card.png cargo test -- --ignored
```

## License

[GPL-3.0](LICENSE)
