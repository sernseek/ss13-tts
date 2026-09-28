# ss13-tts

[简体中文](./README.zh-Hans.md)

A text-to-speech server for Space Station 13. It implements the /tg/station TTS API and synthesizes speech with Alibaba Cloud Model Studio's Qwen-Audio TTS (`qwen-audio-3.1-tts-flash` by default).

Together with the `tts_expression` module of [sernseek/NovaSector](https://github.com/sernseek/NovaSector), it also supports emotion tags, voice descriptions, speaking rate and pitch, and player-made voices (designed from a description or cloned from a recording, both reviewed by admins).

## Running

You need a DashScope API key for the Beijing region.

```sh
cp .env.example .env   # set DASHSCOPE_API_KEY and TTS_AUTHORIZATION_TOKEN
cargo run --release
```

In the game's `config/config.txt`:

```
TTS_HTTP_URL http://127.0.0.1:5002
TTS_HTTP_TOKEN <same as TTS_AUTHORIZATION_TOKEN>
```

The other settings are documented in `.env.example`. Run it on localhost or a private network only; it trusts a single shared token.

## Testing

```sh
cargo test
cargo run --example mock_dashscope   # offline DashScope stand-in
```

## License

MIT
