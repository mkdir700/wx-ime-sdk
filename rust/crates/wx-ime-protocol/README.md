# wx-ime-protocol

微信输入法跨设备协议的 Rust 线格式库：包编解码、控制通道加密、六位配对码、控制请求格式、
剪贴板对象引用，以及普通文件传输的 protobuf 消息。不发起网络请求，不管理身份、存储或宿主机
剪贴板。

`wire` 是重建的 proto2 控制消息，`flurry` 是独立的文件传输消息；字段是否存在以 `Option`
保留。`FILE_DESCRIPTOR_SET` 提供由同一份 schema 生成的描述符，SDK 据此生成 gRPC 绑定，无需
兄弟源码目录。

控制握手使用 secp128r1 ECDH 加 AES-256-ECB/PKCS7；图片对象使用 AES-128-CTR。二者是不同
格式，不可互换。配对码用字符串保存，以保留前导零；密钥和配对码在 `Debug` 中脱敏。

构建要求：Rust 1.92+、`protoc`、提供 secp128r1 曲线的 OpenSSL 开发文件和 C 工具链。
`vendored-openssl` 特性会静态链接从源码编译的 OpenSSL。当前 `publish = false`。
