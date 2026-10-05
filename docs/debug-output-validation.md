# GUI Debug 输出验证记录

## 自动验证

当前工作区在 Linux 上通过 130 项测试：核心单元测试 106 项、核心集成测试 1 项、CLI 9 项、GUI 可跨平台模型测试 14 项。

以下检查通过：

```sh
cargo fmt --all --check
git diff --check
cargo test --workspace --features stt-gui/native-gui
cargo clippy --workspace --all-targets --features stt-gui/native-gui -- -D warnings
cargo clippy --workspace --all-targets --target x86_64-pc-windows-gnu --features stt-gui/native-gui -- -D warnings
cargo build --workspace --target x86_64-pc-windows-gnu --features stt-gui/native-gui
```

Debug 相关测试覆盖：

- Core 日志接收器的安装、替换和释放，以及未订阅时回退 stderr 的分支；回调可重入，回调 panic 不向工作线程传播。
- 单条 GUI 日志最多 8192 个 UTF-8 字节；截断不拆分 UTF-8 字符。
- Audio 与 Rewrite 密钥，以及 URL 用户信息和查询参数值的隐藏，包括常见 JSON 转义和 URL 编码形式；先隐藏再截断。新增原始查询值、小写/混合大小写百分号编码、编码后的普通字符及混用 `+` / `%20` 的回归；不改变凭据字母的大小写敏感性或无关文本。
- GUI 日志缓冲的 2000 行和 1 MiB 上限、清空、快照独立性，以及含 emoji 的 UTF-16 选区偏移重定位。
- 本地 HTTP 服务模拟 Audio 503 重试、Audio 连通性测试 401、Rewrite 429 重试和 Rewrite 连通性成功；验证 Upload 开关、分类、状态码、耗时和重试记录。
- 真实 HTTP 连接提前断开，验证 reqwest 错误中保留的四种原始编码查询凭据不会进入日志接收器；Upload 开关关闭时仍无日志。
- 请求日志不主动输出正常的提示词、输入文本、转录或改写结果。

此前使用仓库固定的 FFmpeg 8.1 源码及 SHA-256，在临时目录构建仅包含 WAV/PCM 的 Linux 静态 libav，执行并通过以下检查。本轮 review 修复未修改原生日志桥接，未重复该原生构建：

```sh
PKG_CONFIG_PATH=/tmp/stt-debug-native-tmp/install/lib/pkgconfig \
  cargo test -p stt-core --features static-libav --test debug_output
PKG_CONFIG_PATH=/tmp/stt-debug-native-tmp/install/lib/pkgconfig \
  cargo clippy -p stt-core --all-targets --features static-libav -- -D warnings
```

该集成测试实际执行 PCM 转换，并通过原生 `av_log` 产生诊断：FFmpeg Debug 关闭时不进入 GUI 接收器，原生错误仍输出到 stderr；开启时转发为 FFmpeg 分类。此为同一个集成测试在原生功能开启后的额外执行，不计入上面的 130 项总数。

C 桥接也通过 MinGW 的 Windows 目标编译，使用 `-Wall -Wextra -Werror -Wno-deprecated-declarations`；最后一项仅排除现有 FFmpeg API 弃用警告。

Windows 开发版已完成 CLI、GUI 编译与链接，未启用 `static-libav`。本次未重建完整 Windows 静态 FFmpeg 发布包，未运行完整编码器矩阵或访问真实服务商。用户已反馈此前版本在 Windows 整体测试中未见异常；本轮脱敏修复已通过自动测试，尚未重新进行 Windows 实机验收。

## Windows 人工验收清单

整体测试反馈不代表以下每个故障场景均已逐项验证。本轮新增的 URL 编码凭据脱敏场景可结合第 2、3 项复核。

1. 在 Debug 页分别开启或关闭四类开关。未保存时继续沿用原配置，包括在两个 API 页使用草稿进行连接测试；保存后重新打开设置，确认新开关生效。
2. 开启对应开关并触发录音、音频转换、快捷键、Audio 请求、Rewrite 请求及两个连接测试；检查时间、分类、状态、耗时、失败、重试和取消记录。
3. 检查日志框只读、等宽字体及共享自定义滚动条。选中文本后 `Ctrl+C` 仅复制选区；“复制全部”复制保留的全部日志并保留原选区和浏览位置；“清空”清除当前缓冲，后续日志继续显示。
4. 底部且未选中文本时，新日志自动跟随；向上滚动、选中文本或拖动滚动条时，不被新日志拉回底部。超过上限淘汰旧行后，仍保留的文本选区和浏览位置应保持对应。
5. 日志页不可见及设置窗口关闭时继续触发日志；重新打开后仍可看到本次运行保留的记录。退出并重启程序后记录清空，不生成日志文件。
6. 在中、英、德、日、法界面及不同 DPI 下检查布局、复制和清空按钮、字体、滚动条。反复开关设置窗口，确认没有残留定时器或控件访问错误。
7. 使用相同配置运行 CLI，确认调试输出继续写入 stderr，文件输出和 stdout 行为不受 GUI 日志框影响。
