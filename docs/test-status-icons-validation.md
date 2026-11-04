# 测试状态 SVG 图标验证

替换范围为 Audio API 的 Test connectivity、Advanced Audio API 的 Test workflow、Rewrite API 的 Test connectivity，以及 Edit prompt 的 Test connectivity。

三份素材原样保存在 `assets/status-icons/` 并嵌入程序：`Success1.svg` 对应成功，`Failed1.svg` 对应失败，`Processing.svg` 对应等待。等待图标保持原素材的背景、十二个圆点及透明度；圆点组顺时针匀速旋转，每 1.2 秒循环一次。

`test_status_icons.rs` 将这份素材的旋转按 20 毫秒分成 60 个静态 SVG 阶段，逐个解析并缓存；每次绘制按实际经过时间选择阶段，定时器延迟不会累积成动画减速。背景圆不参与旋转。像素输出保持预乘 BGRA，按物理尺寸缓存，最多 4 MiB 或 256 项。

每个状态图标使用独立、没有 Tab 焦点的原生子窗口。绘制在缓冲区完成后一次呈现；等待时只重绘该窗口。成功、失败、隐藏及销毁会停止动画定时器。图标保持原有的 20 DIP 布局区域，随 DPI 缩放；Rewrite 的滚动视口和 Prompt 编辑器的滚动内容区分别同步图标与结果文本的位置。

自动测试覆盖：

- 原素材的颜色、透明画布、预乘透明通道，以及 100%／125%／150%／200%／300% 等物理输出尺寸。
- 0、300、600、900、1200 毫秒的动画画面，与原 SVG 中圆点组的相应静态旋转逐像素一致；圆点之外的背景像素保持固定。
- 定时器延迟后的阶段选择、跨周期帧复用、静态图标复用及缓存内存上限。
- Windows 原生测试检查动画只使图标进入重绘区域，焦点和边界不变，隐藏后停止、重新显示后继续，以及成功／失败／空闲状态停止定时器。该原生测试在 Linux 上只进行 Windows 目标编译检查，未执行。

验证命令：

```bash
cargo fmt --all --check
cargo test --workspace --features dictate-gui/native-gui
cargo clippy --workspace --all-targets --features dictate-gui/native-gui -- -D warnings
cargo clippy --workspace --all-targets --target x86_64-pc-windows-gnu --features dictate-gui/native-gui,dictate-gui/static-libav -- -D warnings
```

本次通过 473 项工作区测试、格式检查、Linux 和 Windows GNU 目标的全工作区全目标 Clippy（`-D warnings`），以及启用静态 libav 的 Windows GUI 发布版编译和链接。嵌入的三份 SVG 与用户提供的文件 SHA-256 一致。

后续 Batch Transcription 复用同一份 Processing 动画，并加入原样嵌入的 `Success2.svg`／`Failed2.svg` 作为透明底的完成和失败图标。素材测试也覆盖批量页 16 DIP 的布局尺寸及缩放输出，检查透明背景和绿色／红色笔画；四个测试入口继续使用各自的 `Success1.svg`／`Failed1.svg`。批量页的定时器与列表裁剪验证边界见 [批量转写验证记录](batch-transcription-validation.md)。

实机验收需在四个入口分别检查等待、成功与失败；确认等待图标持续转动，测试结束后停止；滚动、切页和 DPI 改变后与文本对齐，关闭窗口后没有遗留动画。实际 Windows 窗口显示和流畅度仍需实机验证。
