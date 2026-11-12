# OhMyKeymint-R1 1.3.8-302（第三方版本）

> 本仓库是基于 [qwq233/OhMyKeymint](https://github.com/qwq233/OhMyKeymint) 修改的第三方版本，**不是官方版本，也不代表上游维护者**。本版本的问题请反馈至[本仓库](https://github.com/ITxiao6666/OhMyKeymint/issues)。

面向 Android 的自定义 Keystore / KeyMint 实现，提供应用配置、Keybox 管理及配套 WebUI。

## 安装与使用

面向 **Android 12–17、ARM64**。标准 ZIP 使用 KernelSU、APatch 或 Magisk 的模块接口；SKRootPro 使用单独的原生加载格式，安装入口、运行前提与验证范围见[管理器适配](docs/ROOT_MANAGER_COMPATIBILITY.md)和 [SKRootPro 说明](docs/SKROOTPRO.md)。

1. 选择本仓库构建的 `full`、`no-soter` 或 `lite` 安装包，通过对应管理器安装并重启。三个版本使用相同模块 ID，同一时间选择其中一版。
2. 打开 WebUI，选择需要使用 OMK 的应用，按需导入 Keybox。
3. 高级设置及生效条件请参阅[配置指南](docs/CONFIGURATION.md)。

KernelSU 可直接从模块列表打开 WebUI；Magisk 需自行安装 KSUWebUIStandalone 或 WebUI X。
Zygisk 相关功能需另行安装并启用 Zygisk Next。

## 主要功能

- 应用包名、多用户及应用独立安全补丁配置。
- RSA / EC Keybox 导入与证书链信息查看。
- 手动安全补丁设置、已保存配置重放与默认值恢复。
- 云端 Keybox 自动检测、条件下载、SHA-256 校验、原生验证导入与热重载；断网指数退避。
- 使用原 Pixel `bot` feed 的 PIF 适配，固定仅 Play Store Build，以及本机 RKP Keybox 签发。
- 运行状态诊断、主题与语言设置；模块描述按核心服务与云端连接结果更新，并显示版本、末七位哈希或具体失败原因。
- SOTER 本地随机身份、按应用选择与 32/36 位 CPU-ID 覆盖。

| 版本 | 发货功能 |
| --- | --- |
| `full` 完整版 | 核心、自动云端 Keybox、补丁日期、GMS 缓存、应用列表、壁纸、PIF、RKP、SOTER 与全部设置 |
| `no-soter` 去 SOTER 版 | 完整版功能中移除 SOTER 原生实现、入口、监视器和专用权限 |
| `lite` 精简版 | 核心、自动云端 Keybox、补丁日期设置与恢复、GMS 缓存清理、包名添加、背景壁纸 |

精简版按 Rust 编译特性裁剪，包内无 PIF Zygisk、工程显示桥、RKP 签发或 SOTER 实现；界面入口与发货脚本同步裁剪。

Keybox 至少需要一个完整的 RSA 或 EC 条目，私钥必须与证书链匹配。
仅含 EC 条目时，RSA 密钥的认证证书也使用该 EC 私钥和证书链签名。
详细配置位于 `/data/misc/keystore/omk/`，请勿公开密钥。

## 注意事项

- 软件实现不能替代真实 TEE 或生物认证，不保证通过检测或支付认证。
- 使用前请备份重要数据，实验性功能请谨慎开启。
- 卸载会删除 OMK 配置及密钥数据，可能需要相关应用重新登录或注册；禁用模块不会删除这些数据。

## Restarting keymint and injector

需要手动重启服务时，可执行对应命令：

```sh
touch /data/adb/omk/restart.keymint
touch /data/adb/omk/restart.injector
touch /data/adb/omk/restart.all
```

哪些设置需要重启服务或设备，请参阅[生效说明](docs/CONFIGURATION.md#how-changes-are-loaded)。

## 许可与致谢

使用前须阅读并同时遵守 [General License Agreement v4 或后续版本](LICENSE.md) 和 [Oh My Keymint License](LICENSE-2)。
本软件按原样提供，不作任何担保。独立标明的第三方部分仍遵循各自原有许可。

Copyright (C) 2025, 2026 James Clef <qwq233@qwq2333.top>

部分代码来自 [AOSP](https://source.android.com/)，采用 [Apache-2.0](template/AOSP.Apache-license-2.0.txt)。

Copyright 2022, The Android Open Source Project

其他第三方来源与许可见[第三方软件说明](docs/THIRD_PARTY.md)。
