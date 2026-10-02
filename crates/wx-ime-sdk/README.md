# wx-ime-sdk

面向独立微信输入法设备身份的异步 Rust 客户端：六位码配对、设备列表、双向文本和图片剪贴板、
局域网文件传输与接收事件。依赖 `wx-ime-protocol`，供 `wxc`、UniClipboard 等宿主集成；
不读写宿主机剪贴板。

构建要求：Rust 1.92+、Tokio 运行时、`protoc`、OpenSSL 开发文件和 C 工具链（交叉编译需要
目标平台的 OpenSSL）。启用 `vendored-openssl` 特性可静态链接 OpenSSL。当前
`publish = false`。

## 身份与配对

`Client::open(ClientOptions::new(state_dir, device_name))` 创建或恢复身份。一个状态目录只允许
一个活跃客户端；私钥和待确认回执由 SDK 持久化。`Client` 可克隆，共享同一会话。

签发方调用 `request_pairing_code`，展示 `PairingSession::code().expose()`，等
`next_update` 报告 `ConfirmationRequired` 后调用 `confirm`。加入方调用
`join(PairingCode::parse(code)?)` 并 `wait`。`cancel` 取消远端事务。

```rust,no_run
use wx_ime_sdk::{Client, ClientOptions, PairingCode, Result};

async fn join_peer(state: std::path::PathBuf, code: &str) -> Result<Client> {
    let client = Client::open(ClientOptions::new(state, "UniClipboard")).await?;
    let mut pairing = client.join(PairingCode::parse(code)?).await?;
    pairing.wait().await?;
    drop(pairing);
    Ok(client)
}
```

## 剪贴板

- `send_text`：最大 1 MiB UTF-8；256 KiB 及以下内联，更大的通过 COS。
- `send_image`：已编码的图片文件，最大 16 MiB，上传前加密。
- `SendReceipt` 只证明服务端接受。`OutcomeUnknown` 表示最终结果不确定，SDK 不会自动重发。
- `poll_clipboard_once` 拉取一次最新值，没有则返回 `None`；`next_event` 持续轮询直到出现
  `GroupChanged` 或 `ClipboardReceived`。两者都只是“最新值”轮询，不是历史队列。同一时间只有
  一个未确认回执，重启后仍会保留。
- 宿主把内容提交成功后，才调用 `acknowledge(receipt.id)`。图片路径属于 SDK 缓存，确认后会被
  回收，需要先复制走。

## 文件

- `send_file(peer_uin, path, advertised_ips)` 邀请已配对设备，并通过双向认证的局域网 TLS 发送
  一个文件（最大 1 GiB）。完成表示接收方已确认完整范围。
- `pending_file_invite()` 向服务端查询一次发给本设备的文件邀请，返回邀请码和发送者。查询不会
  消耗邀请，调用方需按邀请码去重。
- `receive_file(peer_uin, &code, FileReceiveOptions)` 接受邀请并返回 `FileReceiver`。宿主空闲
  时接收器仍会持续处理控制消息；反复调用 `receive` 领取文件。已完成文件队列容量为 1，宿主
  未领取前一个文件时，下一个文件会被拒绝，发送方收到 `Canceled`。
- 没有广域网中继，公布的地址必须能被对端访问。

## 关闭与诊断

用完调用 `Client::shutdown`：关闭控制和文件会话并释放身份锁。`ClientOptions::capture_dir`
默认关闭；开启后会保存敏感的请求材料，仅用于受控诊断。

## OpenSSL

默认使用目标平台的 OpenSSL。`vendored-openssl` 特性会编译 `openssl` crate 自带的 OpenSSL
源码（需要 C 工具链、Perl 和 make），并转发给 `wx-ime-protocol`。
