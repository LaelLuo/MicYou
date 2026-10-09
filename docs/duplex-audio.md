# MicYou Duplex

Windows电脑声音经局域网传到Android，再由手机蓝牙耳机播放；现有手机麦克风上传继续运行。此fork来自MicYou，保留原作者与许可。

## 使用

1. 解压Windows便携包，使用`Start-MicYou-Duplex.ps1`或其中的`micyou.exe`启动。Duplex身份的GUI默认配置保存在旁边`data/`，首次使用端口18554（UDP18555）。单独从终端启动CLI/TUI时，先将该进程的`MICYOU_CONFIG_DIR`设为这个`data/`绝对路径；通过GUI切换则继承隔离目录。
2. 在Android安装独立的`MicYou Duplex`包。它的包名为`com.laelluo.micyou.duplex`，可与原版同时安装。
3. 桌面选择麦克风输出设备，启动Wi-Fi服务器；手机连接该电脑的18554端口。原版MicYou如仍在使用同一麦克风，不要同时向同一虚拟输入送两份录音。
4. 手机先连好蓝牙媒体耳机；桌面设置的“电脑声音 → 手机”选择实际有电脑声音的播放设备；手机开启“播放电脑声音”。保留手机本体麦克风收音。
5. 关掉该开关仅停止电脑声音。耳机断开或音频焦点丢失后需手动重新开启。重连默认关闭。

选择当前承接手机麦克风的播放设备会被拒绝，防止自己的声音送回耳机。此功能不创建虚拟扬声器，不自动改Windows默认设备。若原音源设备仍发出声音，可在具体播放器选择一个合适的现有输出；不要把系统静音是否影响loopback采集当作各驱动都一致。

首版传输固定48kHz双声道PCM16，每20ms一帧，约1.536Mbps音频载荷。支持声明缺席时保留旧版上行。共享TCP在网络拥塞时可能影响两路；端到端延迟还包含Android和蓝牙缓冲，需按实际耳机验收。Web客户端和蓝牙耳机麦克风不在本版双向范围内。

## 构建

需要JDK21、Android SDK37、Bun仓库指定版本和Rust1.95或更高。保持仓库默认版本源，不修改全局工具默认。

```powershell
# 仓库根目录，给当前进程设JAVA_HOME/ANDROID_HOME后
./gradlew.bat -Pmicyou.duplex=true :composeApp:testDebugUnitTest :composeApp:assembleDebug

# 工作目录tauri-app
$env:RUSTUP_TOOLCHAIN = '1.95.0'
bun install --frozen-lockfile
cargo test --workspace
cargo clippy --workspace --all-targets
bun run build
bun run tauri build --no-bundle --config src-tauri/tauri.duplex.conf.json
```

Windows便携产物应包含micyou.exe、CLI/TUI同目录sidecar、resources、原LICENSE、本说明及启动脚本。Android调试签名包仅用于此独立包名的验证；正式签名发布需另行配置签名材料。原版更新入口不用于升级此fork，请用本fork构建产物。

交付时先提交已审查的源码，在仓库根目录执行`bun scripts/build-duplex.ts`从干净提交构建两端并记录产物哈希。随后执行`bun scripts/package-duplex.ts <新的输出目录>`。打包检查提交、哈希、真实APK包名/版本及签名指纹；缺少或不匹配构建收据即拒绝。构建及打包都需要当前进程的JAVA_HOME、ANDROID_HOME和所需工具PATH。

## 回退

退出MicYou Duplex并关闭其手机上行后，继续使用原版MicYou。原版配置、安装与系统默认音频未由启动脚本修改。可保留整个便携目录以保留来源设置；移除独立Android包不影响原版包。构建通过不等于真人双向试听通过，交付记录需注明真实验收状态。
