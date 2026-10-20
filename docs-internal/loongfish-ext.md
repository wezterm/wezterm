# loongfish-ext — 本仓(wezterm fork)定制扩展与定谳登记

本文件登记本 fork 相对上游的行为差异、扩展决策与问题定谳(症状/证据/根因
file:line/建议),供后续维护与上游同步时对照。体例参照 mdink 仓同名文件。

---

## §1 小键盘 NumLock 数字经 kitty 协议编码为导航键——X11 侧 locked modifiers 恒被清零(2026-10-20 定谳;同日已修复)

### 症状

- `wds agent --tui`(WebDeepseekConnector)启动会话选择屏(会话从 1 编号,
  行式输入编号回车续用):NumLock 开启时小键盘数字输不进;聊天输入框同。
- 官方 AppImage(20260117-154428-05343b38)同场景正常;本仓构建
  (20261016-233326-4a098c97,即 HEAD)复现失效。
- 影响面:本构建下**所有推送 kitty 键盘协议旗标的应用**(不推旗标的应用走
  legacy 明文,不受影响)。

### 取证装置与证据(可复跑;装置存档 /tmp/wds-numpkf/)

Xvfb :82 隔离 + **XTEST 键码注入**(ctypes 直调 libXtst;KP_1=87 KP_5=84
KP_Decimal=91 KP_Enter=104 KP_0=90 KP_7=79,NumLock=77;LED 逐键核验恒
00000002=锁恒开)+ `WEZTERM_CONFIG_FILE` 注入 debug_key_events 配置 +
`wezterm start --always-new-process`:

1. **编码层输出**(debug_key_events 日志,位于写 pane 的同一分支——源头
   级证据):锁恒开下
   - KP_1 → `\x1b[57424;1u`(kitty 57424=**End**;应发 57400=数字 1)
   - KP_5 → `\x1b[E`(光标下移行;应发 57404)
   - KP_Decimal → `\x1b[57426;1u`(**Delete**;应发 57409)
   - KP_0 → `\x1b[57425;1u`(**Insert**;应发 57399)
   - KP_7 → `\x1b[57423;1u`(**Home**;应发 57406)
   - KP_Enter → `\x1b[57414;1u`(Enter,两态同码,不受影响)
   - 全部无 129(numlock)修饰位;释放事件正常抑制(编码为 `""`)。
2. **KeyEvent 全程 `leds: (empty)`**(WEZTERM_LOG=trace):NumLock 键按下
   锁定(服务端 LED 已亮)之后到达的每一个键,KeyEvent.leds 仍为空。
3. **xcb 层有真相**:trace `merge_current_xcb_modifiers(selected);
   state before=StateFromXcbStateNotify { depressed_mods: 0, latched_mods: 0,
   locked_mods: 16, … }`——xcb 上报 locked_mods=16(Mod2=NumLock 锁定),
   数据在手却被丢弃(见根因)。
4. **对照腿**:AppImage 同协议同键码,键以 `Char('1')` 走 legacy 明文
   (`send to pane DOWN key=Char('1')` / Char('5') / Char('.') / Enter /
   Char('0'))——官方版正常的机制:`enable_kitty_keyboard` 默认关。

### 根因链(file:line 均为本仓 HEAD 4a098c97)

1. **根因点**:`window/src/os/x11/keyboard.rs:855-868`
   `merge_current_xcb_modifiers()`:
   ```rust
   self.state.borrow_mut().update_mask(
       mods,                        // xcb 原始掩码(含锁定位)塞进 depressed
       0,                           // latched 恒 0
       0,                           // ← locked 槽硬编码 0:xcb 的 locked_mods 被丢弃
       state.depressed_layout, …
   );
   ```
   xkb state 的 locked modifiers 恒空。:856 已把 StateFromXcbStateNotify
   (含 locked_mods=16)取出来,只是没传进 update_mask。
2. **推导层受害**:同文件 `:556-563` `get_led_status()` →
   `led_name_is_active(xkb::LED_NAME_NUM)`——xkbcommon 的 LED indicator
   (xkeyboard-config "Num Lock" 定义)从 **locked** 态推导 → 恒 false。
   注意:`key_get_one_sym` 走 effective(含 depressed),数字层级解析仍正确
   (KeyEvent.key=Char('1')),唯 LED/locked 消费者受害——这是该缺口长期
   潜伏的原因。
3. **传播**:同文件 `:356` `let leds = self.get_led_status();`
   (process_key_event_impl)→ KeyEvent.leds 恒空。
4. **受害消费者**:`wezterm-input-types/src/lib.rs:1867`
   `match (numpad, self.leds.contains(KeyboardLedStatus::NUM_LOCK))` 恒走
   false 臂(数字键→导航功能码);`:1792` 的 numlock 128 修饰位恒不置。
   既有单测锚定的 `\x1b[57404;129u`(lib.rs:2876)在 X11 真机路径从未达成
   ——单测直接构造 leds 字段,绕过了 X11 层。
5. **下游表现**:crossterm 0.29 把 57424/57426/57425/57423 解码为
   End/Delete/Insert/Home,`\x1b[E` 解析失败被丢——应用收不到任何数字。
6. **分水岭**:commit「config: enable the kitty keyboard protocol by
   default」令本仓默认走 kitty 编码,leds 成为承载性输入,旧有 X11 缺口
   显性化;官方版 legacy 明文不受影响。

### 修复(2026-10-20 已实施;commit 见 git log「x11: keep latched and locked modifiers…」)

- 治本(定谳建议的第一案):`merge_current_xcb_modifiers` 把缓存态的
  `latched_mods`/`locked_mods` 传入 `update_mask` 第二/三参(原先恒 0)。
- 补强:`Keyboard::new()`(xcb 路径)用 `serialize_mods/serialize_layout`
  从 `xkb_x11_state_new_from_device` 应用的服务器初始态**播种
  `last_xcb_state`**——覆盖「wezterm 启动前 NumLock 已锁定」场景
  (libxkbcommon ≥1.4 的 `state_new_from_device` 会 XkbGetState 并
  `xkb_state_update_mask` 应用,播种即读回)。
- 回归锚:`window/src/os/x11/keyboard.rs` 新增单测
  `merge_current_xcb_modifiers_keeps_locked_modifiers`(离线可跑,无 X
  依赖;已验证对修复前代码 FAIL、修复后 PASS);既有 encode_kitty 单测
  (input-types 18 个,含 `\x1b[57404;129u` 锚)全绿。

### 修复验证(Xvfb + XTEST 装置,存档 /tmp/wds-numpkf-fix/)

三腿对照,证据等级同定谳(encode 分支日志,写 pane 同源):

| 腿 | 二进制 | 场景 | KP_1 | KP_5 | leds |
|---|---|---|---|---|---|
| before | 已装 4a098c97(=HEAD) | 会话中锁 NumLock | `57424;1u`(End) | `\x1b[E` | 空 |
| after-toggle | 修复版 release | 同上 | `57400;129u` ✅ | `57404;129u` ✅ | NUM_LOCK |
| after-prelock4 | 修复版 release | 启动前已锁定 | 首键即 `57400;129u` ✅ | `57404;129u` ✅ | NUM_LOCK |

- prelock4 腿首个 `merge_current_xcb_modifiers` 之前 **0 次
  `update_state`**(无任何 StateNotify),`state before={locked_mods: 16}`
  纯来自启动播种——播种路径独立证实。
- 全键矩阵(after-toggle):KP_0→57399、KP_7→57406、KP_Enter→57414,
  均带 129;KP_Decimal(键码 91)→ `57426;129u`:本 keymap 下 phys 名为
  KeypadDelete("KPDL"),表驱动 `_` 臂与 LED 无关——定谳「应发 57409」
  以 KeypadDecimal 命名为前提,两版行为一致,非本 bug 范畴。
- 窗格内裸 `cat` 嗅探两版仍对称零字节(同定谳「未定谳」项,不影响结论)。
- **装置已固化入库**(2026-10-20,commit「ci: add an X11 keyboard
  integration test」):`ci/test-x11-keyboard.sh` 自包含三腿断言(锁开→数字
  /锁关→导航的反向对照/启动前已锁→纯播种核验,15 断言),helper
  (xkey.py/conf.lua/sniff.sh)以 heredoc 内嵌;配手写 workflow
  `.github/workflows/x11_keyboard_test.yml`(apt 装 xvfb/xdotool/x11-utils/
  libxtst6,构建 debug 版跑),CONTRIBUTING.md 增「Testing the X11
  keyboard path」小节。已验证:修复版 15/15 PASS,修复前二进制腿1即
  FAIL 退出 1。后续复跑以仓内为准,/tmp/wds-numpkf* 仅作历史存档。

### 修复建议遗留(未实施,低优先)

- 或:`xkb_state_update_key` 喂键码自维护本地 state(kitty 终端做法);
- 或:`get_led_status` 改查 XKB 扩展 indicator 状态(XkbGetIndicatorState)。

### 取证装置坑(复用注意)

- **xdotool 键名注入会自动切换 NumLock** 来合成键符(`xdotool key KP_1`
  注入前把锁翻面——状态日志逐键可见 ON↔OFF 翻转),导致键以锁关态到达,
  编出导航码是「正确响应错误输入」的假象;必须改 **XTEST 键码注入**。本机
  xdotool 无 `key --keycode`,用 ctypes 直调 libXtst(/tmp/wds-numpkf/xkey.py)。
- **`xset q`/`xdotool` 的 DISPLAY 陷阱**:裸 shell 未 export DISPLAY 时默认
  连 :0(用户真机)——本轮曾读出假绿 LED=00000002(真机恰好锁开)。必须
  `export DISPLAY=:N`;xdotool 的 `--display` 是全局选项,**必须放在子命令前**
  (`xdotool --display :N windowfocus W`),放后面静默失效。
- **裸 Xvfb 无聚焦窗口时服务器不处理 XTEST 按键**(锁定动作不发生);
  prelock 类「先设锁后启动被测端」的腿,需先借一个聚焦的 xev 窗口按
  NumLock(xev 事件 `state 0x10` 为锁定生效的权威判据)。
- **杀掉持焦点的客户端会让 Xvfb 重置键盘锁定态**:设锁的 xev 必须全程
  存活到被测腿结束,否则会误判「wezterm 启动清了服务器锁」(本轮曾三度
  误判,机制为焦点归 None 时服务器重置键盘态;真实 Xorg 桌面无此现象,
  lxterminal/GTK 客户端同样触发,非 wezterm 特有)。
- **ctypes 直调 `XkbLockModifiers` 必须先 `XkbUseExtension`**,否则静默
  无效(仍返回 True);`XkbGetState` 同理直接 False。本轮放弃该路径,
  改用 xev+XTEST 设锁。
- **xev 输出全缓冲**:SIGTERM 杀掉时尾部事件丢缓冲,用 `stdbuf -oL xev`。
- 配置注入的环境变量是 `WEZTERM_CONFIG_FILE`(`WEZTERM_CONFIG` 无效——
  会静默加载用户真实 `~/.wezterm.lua`;trace 首行「consider config」可辨;
  本轮各腿因此实际跑在用户真实配置下,保真度反而更高)。
- 收尾用 `xdotool windowkill`(优雅关 pane,日志冲盘);`kill Xvfb` 会
  猝死 GUI 丢缓冲。GUI 进程 PID 取自窗口 `_NET_WM_PID`
  (`xdotool getwindowpid`),比 pgrep 模式匹配可靠(避免误中用户在跑的
  同名进程)。窗格内 `cat > file` 裸嗅探在本装置两版对称零字节
  (未定谳;不影响结论——encode 源头日志证据等级更高,v1 腿的管道末端
  字节与 encode 日志互证一致)。
- Xvfb 无 WM:`windowactivate` 报 `_NET_ACTIVE_WINDOW` 错无害,
  `windowfocus` + 唯一窗口即可收键。
