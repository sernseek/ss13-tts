# ss13-tts

[English](./README.md)

给 SS13 用的文字转语音服务。实现了 /tg/station 的 TTS 接口，后端调用阿里云百炼的 Qwen-Audio TTS（默认 `qwen-audio-3.1-tts-flash`）。

配合 [sernseek/NovaSector](https://github.com/sernseek/NovaSector) 的 `tts_expression` 模块，还支持情绪标签、声音描述、语速音高，以及玩家定制音色（文字设计 / 录音复刻，需管理员审核）。

## 运行

需要北京地域的 DashScope API Key。

```sh
cp .env.example .env   # 填 DASHSCOPE_API_KEY 和 TTS_AUTHORIZATION_TOKEN
cargo run --release
```

游戏的 `config/config.txt`：

```
TTS_HTTP_URL http://127.0.0.1:5002
TTS_HTTP_TOKEN <和 TTS_AUTHORIZATION_TOKEN 一样>
```

其他配置项见 `.env.example`。只在本机或内网运行，它只认一个共享令牌。

## 测试

```sh
cargo test
cargo run --example mock_dashscope   # 离线模拟 DashScope
```
