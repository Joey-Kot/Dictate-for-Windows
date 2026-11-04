# GUI 批量转写验证记录

## 实现边界

- 设置侧栏在 Cache 与 Debug 之间新增 Batch Transcription。页面包括较高的文件列表、输出目录、默认值为 1 的 Batch Size，以及 Files、Folders、Transcribe/Cancel、Retry。
- 文件列表支持 Ctrl 逐项多选、Shift 区间选择和 Delete 单项/批量移除。删除只操作列表，源音频和已有结果保留；正在处理或等待名额的文件会单独取消，未派发文件跳过，其他文件继续。移除后重新导入使用新 ID，旧任务结果不会更新新行。
- 文件多选、文件夹多选及递归扫描、文件拖放共用音频扩展名表。路径去重，跳过目录链接和 Windows 目录重解析点。后台扫描，列表按定时器分批加入；运行期间新增项供下一轮使用。
- Batch Size 限制并行处理的文件数量。Network 的 Upload concurrency 是所有文件与分段共享的完整识别工作流上限，普通文件、Advanced 实时文件回放同样受限。名额覆盖上传、请求、重试、轮询、取结果和远程清理；分段文件不在外层重复申请名额。
- Transcribe 处理启动时的列表；Retry 仅重试 Failed 项，且整个任务及清理结束前始终禁用。取消停止派发并等待已启动工作流清理，保留完成结果和失败状态，未完成项恢复待处理。
- 每个文件拥有独立临时目录，不执行根目录清理。结果先写入同目录临时文件，再以不覆盖方式提交；重名增加编号。源文件保留。CLI 共享处理逻辑，保留原来的默认输出路径及显式覆盖行为。
- API 返回合法空文本会写出空文件；本地分析无语音标记完成但不生成文本。分段失败不输出部分文本。输出目录不可用或任务被拒绝时，保留此前失败项以供重试。
- 开始时使用设置草稿快照，Debug 开关沿用已保存值。运行时锁定输出目录、Batch Size、Upload concurrency、Save。切页继续运行；关闭设置或退出应用先取消并等待清理。
- `BATCH_TRANSCRIPTION` 只保存 `output_dir` 和正整数 `batch_size`，默认分别为空值和 1；空目录设置由 GUI 解析为 Windows Documents/Transcripts。队列不持久化。

## Windows 测试反馈与重绘调整

用户已反馈批量转写功能基本正常，并报告转写时界面抖动、状态内容未居中。本轮针对这两项调整：

- 行内容先在离屏 DC 完整绘制，再一次提交；后台动画只使可见 Processing 行的图标区域失效，不再定时刷新整行。
- 当时使用固定圆点轮换亮度；后续的 SVG 替换改为下文所述的圆点组旋转，图标布局区域仍保持固定。
- Processing、Completed、Failed 使用实际字体测量后的宽度，将图标与文字整体水平居中，文字保持原生垂直居中。
- 新增跨 DPI、不同文字宽度的居中及动画区域测试；原生 GDI 测试验证离屏构图期间目标像素不变，提交遵守动画区域裁剪。GDI 测试已通过 Windows 目标编译，仍待 Windows 执行。

此次修复后的动画与居中效果尚未在 Windows 实机重新验证。

## 批量状态 SVG 替换

Processing 使用用户提供的 `Processing.svg`，保留深色背景、十二个白色圆点、透明度梯度，以及 1.2 秒一圈的顺时针旋转。Completed 使用透明底绿色对勾 `Success2.svg`，Failed 使用透明底红色叉号 `Failed2.svg`。三份文件按原始内容嵌入程序，通过共用的 SVG 渲染缓存生成适配 DPI 的像素。

列表沿用 16 DIP 图标布局和图标／文字整体居中。独立的 20 毫秒动画定时器只刷新可见 Processing 行的图标矩形，切页或批次结束后停止；工作线程和导入事件仍按 50 毫秒轮询。渲染继续使用行缓冲及目标裁剪。

素材验证扩展到批量页的 16 DIP 及多个缩放尺寸，检查成功／失败图标的背景透明、实际笔画颜色及预乘透明通道。已有旋转周期、帧缓存、居中布局和仅图标区域重绘的覆盖继续保留。原生列表动画、切页恢复和多行显示仍需 Windows 实机复核。

本次 SVG 替换通过 473 项工作区测试、格式检查、Linux 和 Windows GNU 目标的全工作区全目标 Clippy（`-D warnings`），以及启用静态 libav 的 Windows GUI 发布版编译和链接。三份嵌入素材与用户本次提供的文件 SHA-256 一致。

## 右侧留白和深色长条修正

批量列表此前始终预留 14 DIP 滚动条位置，且滚动条使用下拉列表的深色底色，与输入框背景不同。内容未超出一屏时也会出现多余留白和深色长条。

现在按实际条目数与可见行数决定是否显示滚动条、预留宽度。导入、删除、DPI 布局及切换设置页都会同步；删除到一屏以内后恢复列表完整内宽，并重置顶部索引。滚动条底色与列表输入框背景一致。

使用 Wine 10／Xvfb 执行实际 GUI 发布程序，通过 Files 对话框导入本地占位文件，核对空列表、两条记录、十二条记录、滚动后删除至两条，以及切页返回。100% DPI 下列表右边界在无需滚动时为 726 像素，溢出时为 712 像素，删除后恢复到 726 像素且顶部索引为零；实际 GDI 截图中两条记录的右侧深色长条已消失，溢出时显示正常滚动条。该流程只验证列表交互和显示，没有进行转写或网络请求。

通过 56 项 GUI 测试、格式与差异检查、Linux／Windows GNU 全工作区全目标 Clippy（`-D warnings`）及静态 libav Windows GUI 发布版编译。上述截图属于兼容环境，Windows 实机和高 DPI 的最终视觉效果仍需复核。

## 默认文档目录缺失时的初始化修正

审查发现，默认输出目录为空时，设置页在初始化过程中用 `KF_FLAG_DEFAULT` 查询 Documents。目录缺失或不可访问会使查询失败，并提前中断控件创建；关闭错误提示后再保存，会因缺少批量页控件而触发 panic。

现在使用 `KF_FLAG_DONT_VERIFY` 只解析路径，不在打开设置时验证或创建目录。若路径解析本身失败，仍完成全部控件初始化，并保留空白、可编辑的输出目录。空输出配置可以保存；点击 Transcribe 时检查空路径并提示用户。实际运行沿用核心的 `create_dir_all`，递归创建缺失的 Documents 和 Transcripts，再检查可写性；失败时在识别请求发送前返回错误。

新增核心回归覆盖 Documents 及其 Transcripts 子目录均不存在时，启动批次自动创建两级目录并正常输出。474 项工作区测试、格式与差异检查，以及 Linux／Windows GNU 全工作区全目标 Clippy（`-D warnings`）通过，Windows 静态 libav GUI 发布版构建成功。

使用 Wine 10／Xvfb 执行实际 GUI 发布程序，验证 Documents 缺失时完整打开设置、保存空输出配置后重新打开、点击 Transcribe 才提示空目录、实际转写递归创建两级目录并写入文本，以及目标路径被普通文件占用时提示错误且程序继续运行。测试使用本地模拟服务，全部流程只发送一次识别请求，源音频和已有目标文件保持原样；隔离环境的配置与 Documents 目录均已恢复。中英文 README 的 SHA-256 与本次修改前一致。

## 自动验证

此前批量转写及重绘调整的工作区 467 项测试全部通过；启用静态 libav 并包含 GUI 模型后的 477 项测试也全部通过。测试使用本地 TCP 模拟服务，不调用真实服务商，也不需要 API 凭据。

覆盖内容：

1. Legacy 与 Advanced 普通请求中，Batch Size 大于 Upload concurrency 时仍遵守共享上限。
2. 多个文件的多个分段共用上限；Batch Size 为 1 时仍允许该文件在上限内并行识别多个分段。
3. Advanced WebDAV 上传、异步提交、轮询、DELETE 清理完整生命周期共用一个名额，下一项不能在清理完成前开始上传。
4. 取消正在识别及等待名额的文件，停止后续派发，清理全部已启动临时资源并释放 Runtime 任务锁；UI 在无异步让出的本地准备阶段取消时，也不会被无语音成功结果覆盖。
5. 单文件失败不停止其他文件；空响应、本地无语音、同名输出避让、源文件保留及同秒缓存归档不冲突。
6. 递归扫描重叠目录去重、扩展名过滤、链接循环和取消。
7. GUI 队列的四种原型状态、运行期间禁用 Retry、仅重试失败项，以及启动失败后恢复原状态。
8. 旧配置默认值、新配置往返，以及零值、负数、小数和溢出的批量大小校验。
9. 单项、多项及全列表移除，重新导入、删除后的状态更新、失败项和启动失败回滚；移除不删除源文件或已完成文本。
10. 运行中同时移除正在识别、等待名额和未派发的文件，验证其他文件正常输出且临时资源清理完成。原生 LISTBOX 多选、删除、焦点及清空测试已通过 Windows 目标编译，尚未在 Windows 执行。

11. 图标与文字整体居中，覆盖不同标签宽度及 100%、125%、150%、200% DPI；动画失效区域只包含图标，不包含文件名及状态文字。

执行的检查和构建：

```sh
cargo fmt --all --check
git diff --check
cargo test --workspace --features dictate-gui/native-gui
cargo clippy --workspace --all-targets --features dictate-gui/native-gui -- -D warnings
cargo clippy --workspace --all-targets --target x86_64-pc-windows-gnu --features dictate-gui/native-gui -- -D warnings
JOBS=4 bash scripts/test-embedded-audio.sh
PKG_CONFIG_PATH="$PWD/native-audio-test-tmp/ffmpeg-9.0.2/install/lib/pkgconfig" \
DICTATE_AUDIO_FIXTURES="$PWD/native-audio-test-tmp/ffmpeg-9.0.2/fixtures" \
CARGO_TARGET_DIR="$PWD/native-audio-test-tmp/ffmpeg-9.0.2/target" \
cargo test --workspace --features dictate-gui/native-gui,dictate-core/static-libav
bash scripts/build-ffmpeg-windows-amd64.sh
bash scripts/build-rust-windows-amd64.sh
bash scripts/package-windows-release.sh
```

Linux 原生音频验证使用仓库固定版本和 SHA-256 的 FFmpeg 源码，实际执行音频转换及现有音频夹具测试。Windows 交叉编译覆盖原生设置页和文件选择 API；Linux 测试不执行 Windows 控件。

## Windows 实机验收：待执行

以下项目尚未在 Windows 桌面执行，构建成功不等同于视觉和交互验收。

1. 复核转写期间文件名、选中背景和状态标签不闪烁，Processing 图标内部按素材旋转，Completed 为透明底绿勾，Failed 为透明底红叉；三种状态的图标和文字在标签内整体居中。逐一对照四张原型：空列表、导入后三个文件、Completed/Failed/Processing 混合运行状态，以及 Completed/Failed/Completed 后 Retry 可点击的状态。
2. 确认 Files/Folders 多选、递归、拖放、重复导入、长文件名、中文路径、文件详情和错误提示、列表选中及滚动。
3. 在 Batch Size 大于 Upload concurrency 时转写普通文件及分段文件，结合服务端记录确认整个批次未超过上限。关闭分段开关后 Upload concurrency 仍可编辑并生效。
4. 测试正常结束、失败重试、等待名额时取消、转换中取消、识别中取消和远程清理；运行结束前 Retry 保持禁用。
5. 运行时切换设置页、新增文件、关闭设置、按 Esc、退出应用，确认清理完成后关闭且没有悬空控件访问。重新打开设置后队列为空，已保存目录和批量大小保留。
6. 检查中、英、德、日、法界面以及 100%、125%、150%、200% DPI 下的标签、状态徽标、按钮、提示信息和滚动条。
7. 检查单选 Delete、Ctrl 不连续多选、Shift 区间多选、全选范围移除、删除后相邻行选择和滚动条更新；按住 Delete 不应因按键重复误删后续行。运行中移除文件后其他文件继续，同名文件重新导入后不接收旧任务状态。
8. 使用真实音频分别验证 Legacy、Advanced 请求、流式响应、异步轮询和实时文件回放。确认结果文件完整、重名不覆盖、源音频保留。
