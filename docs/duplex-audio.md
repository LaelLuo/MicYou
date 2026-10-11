# MicYou Duplex

Windows电脑声音经局域网传到Android，由手机当前系统媒体输出播放（扬声器、有线或蓝牙耳机）；手机本体麦克风继续上传。此fork来自MicYou，保留原作者与许可。

## 使用

1. 解压Windows便携包，使用`Start-MicYou-Duplex.ps1`或其中的`micyou.exe`启动。Duplex身份的GUI默认配置保存在旁边`data/`，首次使用端口18554（UDP18555）。单独从终端启动CLI/TUI时，先将该进程的`MICYOU_CONFIG_DIR`设为这个`data/`绝对路径；通过GUI切换则继承隔离目录。
2. 在Android安装独立的`MicYou Duplex`包。它的包名为`com.laelluo.micyou.duplex`，可与原版同时安装。
3. 按下一节配置两条独立通道，启动Wi-Fi服务器；手机连接该电脑的18554端口。原版MicYou如仍在使用同一麦克风，不要同时向同一虚拟输入送两份录音。
4. 桌面“电脑声音 → 手机”选择Hi-Fi Cable Input；手机开启“播放电脑声音”。声音按手机系统当前选择播放，普通耳机/扬声器切换继续播放，拔耳机后可能接续外放。
5. 关掉该开关仅停止电脑声音，手机麦克风继续上传。其他应用抢占音频焦点或MicYou重连后，手动重新开启；重连默认关闭。

## 电脑声音只在手机播放

使用两条虚拟线，分别承接电脑声音和手机麦克风，不需要Steam。普通VB-CABLE和HIFI-CABLE/ASIO Bridge均从[VB-Audio官网](https://vb-audio.com/Cable/index.htm)独立下载和安装，保留厂商来源与donationware说明；本包不包含驱动安装器。以管理员权限运行各自原安装器，按提示安排重启，安装后核对Windows默认设备，因为安装器可能自动更改默认。

| 用途 | 选择 |
| --- | --- |
| Windows默认播放、默认通信播放，目标应用的声音输出 | Hi-Fi Cable Input |
| MicYou“电脑声音 → 手机” | 同一个Hi-Fi Cable Input播放端 |
| MicYou“手机麦克风 → 电脑录音” | 普通CABLE Input或CABLE In 16ch（按实际可用端点选择） |
| Windows默认录音、电脑通话应用的麦克风 | 普通CABLE Output |

在Windows设备属性中将HIFI播放端和录音端均设为48kHz。保持Hi-Fi Cable Output的“侦听此设备”关闭，不运行ASIO Bridge向实体声卡转播。MicYou从HIFI播放端采集；不要选择HIFI录音端作为电脑通话输入。普通CABLE的2ch与16ch是同一条线的不同入口，不能用它们分别混合媒体与麦克风。

保存原默认设备和目标应用的显式输出设置，再做上述一次性配置；MicYou不会每次启动强制改系统默认。已经固定到物理扬声器的应用需要单独改输出，必要时重新打开该应用的音频会话。电脑未在其他地方转播这些声音时，HIFI自身不会从电脑物理扬声器发声。

Windows设备绑定保存稳定ID，改设备名称不改变绑定；重装驱动导致设备ID变化时重新选择。上行与下行属于同一条虚拟线会被拒绝，未知虚拟组件无法确认隔离时会显示原因。端点缺失不会悄悄将麦克风送进默认下行通道。已检测到设备不等于实机听觉验收完成。

## 耳返与回声消除

电脑端主动打开耳返，会把手机麦克风声音播放到电脑当前默认输出。默认输出设为HIFI时，你能从手机同时听到媒体与自己的延迟声音；耳返不会因为与电脑声音来源相同而关闭下行。

耳返开启且与回声消除参考重合时，回声消除暂时停止并显示原因，保存的开关不变；关闭耳返后参考准备成功会恢复。手机外放可能被手机麦克风重新拾取，尤其外放与耳返同时开启时可能形成回声或啸叫。按实际需要调低音量或关闭耳返；应用允许用户选择扬声器和耳机。

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

关闭手机下行和上行，退出MicYou Duplex，恢复本次保存的Windows默认设备、目标应用输入/输出及MicYou配置，再继续使用原版或旧版Duplex。配置可连同便携目录中的data一起备份。正常回退无需卸载HIFI；若确需移除，在Windows已安装应用中找到“ASIO Bridge and Hi-Fi Cable”，核对对象后使用其官方卸载入口，按提示安排重启，保留普通CABLE。

原版配置和安装与此独立包分开；移除独立Android包不影响原版包。构建通过不等于真人双向试听通过，交付记录需注明实际验收状态；无Steam环境也需实际核验驱动加载、两条线身份和声音隔离。
