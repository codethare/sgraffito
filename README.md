# sgraffito

Wayland 桌面上的壁纸层涂鸦 / 备忘录：注解画在壁纸之上、普通窗口之下，锁定时鼠标键盘完全穿透。

## 环境要求

需要支持 `wlr-layer-shell` 的合成器：

- 支持：sway、niri、hyprland、river（wlroots / 同类实现）
- **不支持：GNOME（Mutter）、KDE（KWin）** —— 它们不实现该协议
- 可选：fcitx5 等输入法，用于中文输入（走 `zwp_text_input_v3`）

## 构建与运行

```sh
cargo build --release
./target/release/sgraffito daemon     # 常驻进程，由合成器 autostart 拉起
```

控制端（连到 `$XDG_RUNTIME_DIR/sgraffito.sock`，daemon 没跑时会报错退出）：

```sh
sgraffito toggle   # 锁定 <-> 编辑
sgraffito edit     # 进入编辑模式
sgraffito lock     # 回到锁定模式
sgraffito clear    # 清空全部注解
```

合成器快捷键示例：

```
# sway
exec sgraffito daemon
bindsym $mod+d exec sgraffito toggle
bindsym $mod+Shift+d exec sgraffito clear

# niri (~/.config/niri/config.kdl)
spawn-at-startup "sgraffito" "daemon"
binds { Mod+D { spawn "sgraffito" "toggle"; } }

# hyprland
exec-once = sgraffito daemon
bind = SUPER, D, exec, sgraffito toggle

# river (init 脚本里)
sgraffito daemon &
riverctl map normal Super D spawn 'sgraffito toggle'
```

## 用法

**锁定模式**（默认）：注解显示在壁纸层，input region 为空，鼠标键盘完全不经过它。

**编辑模式**：全屏 overlay 抓指针和键盘。

| 按键 | 作用 |
|---|---|
| 鼠标拖动 | 自由划线 |
| `P` | 画笔 |
| `E` | 橡皮（整条线 / 整个文本框为删除单位，命中阈值 8 逻辑像素） |
| `T` | 文本工具，点击落点建立文本框 |
| `1`–`5` | 切换颜色 |
| `Esc` | 结束文本编辑（若有）并回到锁定模式 |

文本编辑中，可打印字符与 `Backspace`、`Enter`（换行）都作用于文本框，此时 `P`/`E`/`T`/数字键是正文而不是快捷键；要换工具就先按 `Esc` 回锁定模式，再 `sgraffito edit` 进入编辑模式后按键切换。

## 数据

`$XDG_DATA_HOME/sgraffito/annotations.json`（默认 `~/.local/share/sgraffito/`）：

```json
{"version":1,"outputs":{"eDP-1":{"strokes":[{"color":"#e01b24","width":3,"points":[[12,40],[14,44]]}],
 "texts":[{"x":100,"y":200,"color":"#ffffff","size":18,"text":"买牛奶"}]}}}
```

- 坐标是 **输出局部逻辑坐标**，按 `wl_output` 的 name 分桶。
- 改动后最多 1 秒落盘，写入走同目录临时文件 + `rename`；`clear` 与退出时立即落盘。
- 文件损坏或版本不支持时，原文件被改名为 `annotations.json.bak` 并以空注解启动。
- 改过输出名（如换线、改名）后，旧分桶的注解不再显示但不会删除。

## 已知限制

- v1 没有 undo/redo、选中/移动/缩放、图层、导出 PNG、工具栏 UI、配置文件、压感/触控。
- 每个输出一份注解，输出改名或拔插后旧注解不显示（数据仍在文件里）。
- 每帧整屏重绘并全屏 damage，4K/高刷下 CPU 占用待实测；若吃紧改为按包围盒局部 damage。
- 进入文本编辑依赖输入法的 `commit_string`；若合成器没有 `text-input-v3`，会退化为本地按键（只能输入键盘布局能直接产生的字符，无候选词）。
- **未经实测**：niri / hyprland / river 上的 `exclusive` keyboard 与 `text-input-v3` 组合、以及 fcitx5 中文选词。首次在真实会话里使用请确认这两点。

## 开发

```sh
cargo test                # 纯逻辑单测：数据模型、命中测试、JSON、渲染、按键映射、命令解析
scripts/smoke.sh          # headless sway 端到端：渲染、模式切换、IPC、clear、单实例、优雅退出
```

规格与设计在 `openspec/changes/sgraffito-v1/`。
