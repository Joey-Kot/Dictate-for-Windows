# 快捷键录入验证

以下验证结果保留自更名前的记录；命令中的包名已同步为当前的 `dictate-*`，不表示本次重新执行。

## 自动验证

核心录入测试覆盖允许按键与全部修饰键组合的配置往返、符号键和小键盘映射、禁止键污染整个组合、纯修饰键、多普通键、长按重复、不同松键顺序、进入焦点时的残留按键及放弃未完成候选。

```bash
cargo fmt --all --check
cargo test --workspace --features dictate-gui/native-gui
cargo clippy --workspace --all-targets --features dictate-gui/native-gui -- -D warnings
cargo clippy --target x86_64-pc-windows-gnu -p dictate-gui --features native-gui -- -D warnings
```

以上检查在 Linux 环境运行；Windows 目标另完成了启用 `native-gui,static-libav` 的 GUI 编译和链接。自动测试不代表 Windows 键盘钩子、实际焦点或绘制效果已经实测。

## Windows 人工验收（待执行）

分别在 `HOTKEY_HOOK=true` 和 `false` 下执行：

1. 在三个输入框分别录入 `F1`、`Ctrl + Alt + S`、`Alt + Esc`；实时预览，全部松开后确认；录入期间不开始录音、不暂停、不取消、不切换窗口。
2. 验证单字母、数字、空格、符号、`Shift + =`、小键盘数字及运算键；保存并重启后触发对应动作。
3. 遍历 README 列出的禁止键，并搭配 Ctrl/Shift/Alt；原值不变。按住 Windows 键再按 A，不能误录成 A。
4. 只按修饰键、同时按两个普通键、长按普通键、先松开修饰键、先松开普通键；均符合录入规则。
5. `Tab`、`Shift + Tab` 正确切换焦点，不产生错误绑定；进入框时已按住的键全部松开后，可以正常录入。
6. 按住组合时点击其他字段、切换设置页、切换窗口或关闭设置；未完成的组合被丢弃，释放按键后应用快捷键恢复，无误触发或修饰键卡住。
7. 录入重复组合，页面提示两个冲突动作，保存被阻止；修改冲突字段后提示消失。
8. 保存后重新打开及重启，值一致；取消设置时不改变原配置。旧配置中的 `win+enter` 等仍可显示并原样保存。
9. 切换中英文，核对提示；在不同 DPI 下检查三个输入框和下方提示不重叠、不裁切。
10. 验证非美式键盘布局、Num Lock 状态及固件 Fn 转换。配置按 Windows 上报的虚拟键识别，符号标签采用美式名称；Fn 本身通常不独立上报。
11. `RegisterHotKey` 模式下使用被其他程序占用的组合，确认应用设置时显示注册失败，没有将录入成功当作生效成功。

本次修改保留原有低级钩子的修饰键匹配语义：要求配置中的修饰键按下，但允许额外修饰键。它与 `RegisterHotKey` 的匹配行为不同。
