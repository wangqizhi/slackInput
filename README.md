# SlackInput

> 🎮 一个帮你优雅摸鱼的 Windows 小工具

上班偷偷玩游戏？老板来了手忙脚乱切桌面？SlackInput 让你按下 Xbox 手柄的 `Guide` 键，一键切换虚拟桌面，丝滑转场，毫无破绽。

## 它能干什么

- 监听 Xbox 手柄的 **Guide（Xbox Logo）** 按键
- 自动触发你配置的键盘快捷键（默认 `Ctrl+Win+Left`，即切换到左侧虚拟桌面）
- 支持自定义映射：`Alt+Tab`、`Ctrl+Shift+Esc`、`Win+D` 等任意组合
- 支持中英文界面
- 后台常驻，带 GUI 配置面板

## 系统要求

- **Windows 11**（当前仅支持）
- Xbox 无线手柄（通过 Xbox Wireless Adapter 或蓝牙连接）

## 快速开始

### 直接使用

从 [Releases](https://github.com/nicq-dev/xbox-guide-mapper/releases) 下载 `SlackInput-win11.exe`，双击运行即可。

### 从源码编译

```powershell
# 安装 Rust 工具链 (https://rustup.rs)
cargo build --release
# 产物在 target/release/SlackInput.exe
```

## 使用方法

1. 运行 `SlackInput.exe`，弹出配置窗口
2. 确保手柄已连接（窗口会显示连接状态）
3. 选择预设组合键，或手动输入自定义映射
4. 勾选「启用输入捕获」
5. 点击「保存」
6. 按下手柄上的 **Xbox** 键，即可触发对应快捷键

### 支持的按键格式

```
Ctrl+Win+Left       ← 默认：切换到左侧虚拟桌面
Ctrl+Win+Right      ← 切换到右侧虚拟桌面
Alt+Tab             ← 切换窗口
Ctrl+Shift+Esc      ← 打开任务管理器
```

支持的修饰键：`Ctrl`、`Alt`、`Shift`、`Win`  
支持的功能键：`F1`-`F24`、方向键、`Tab`、`Esc`、`Enter`、`Space`、`Home`、`End`、`PageUp`、`PageDown`  
支持的字符键：`A`-`Z`、`0`-`9`

## 配置文件

配置自动保存在 `%APPDATA%\SlackInput\SlackInput.ini`。

## 注意事项

- Xbox Game Bar、Steam Input 或某些厂商驱动可能会拦截 Guide 键，导致本工具收不到按键事件。如遇此情况，请先关闭相关软件。
- 当前仅支持 Xbox Wireless Controller（VID: 045E, PID: 02E0）通过 Raw Input 上报 Guide 键。
- 开启「调试日志」可查看原始 HID 数据，方便排查问题。

## License

MIT
