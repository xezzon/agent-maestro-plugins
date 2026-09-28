# zed

[Maestro](https://github.com/xezzon/agent-maestro) 的 Zed 插件：把 Maestro 中录入的 Provider 投影进 `~/.config/zed/settings.json`。

构建、调试与发布见仓库根 [README](../../README.md)。

## 投影规则

每个单协议 Provider 写入 `language_models.<section>."maestro-<slug>"` 一条：

| Maestro | Zed `settings.json` |
| --- | --- |
| Provider slug `foo` | 条目键 `maestro-foo` |
| 协议 `openai-completions` | 段落 `language_models.openai_compatible` |
| 协议 `anthropic-messages` | 段落 `language_models.anthropic_compatible` |
| Base URL | `api_url` |
| API Key | **无处可写**，见下 |
| Model ID | `available_models[].name` |
| Model 显示名 | `available_models[].display_name`（空则省略） |
| — | `available_models[].max_tokens = 131072`（固定默认值，见下） |
| — | `available_models[].capabilities` 不写（Zed 该字段有默认值，Maestro 也不携带能力信息） |

## API Key 无法投影

Zed 的 `openai_compatible` / `anthropic_compatible` 条目没有 `api_key` 字段，Maestro 中录入的凭证因此**落不了盘**——投影后 Zed 侧仍是「未配置凭证」状态。Zed 只在两处取凭证，都需要人工补一次：

1. **在 Zed 的模型/provider 设置里填一次 key**。Zed 把它存进操作系统钥匙串，按 `api_url` 索引；只要 `api_url` 不变，之后的投影（改模型列表、增删 Provider）都不会影响它。改动 Provider 的 base URL 会让钥匙串里的条目对不上，需要在 Zed 里重填一次。
2. **导出环境变量**后重启 Zed：变量名为 provider id 经 `convert_case` 的 UpperSnake 转换后加 `_API_KEY`。例如（实测 `convert_case` 0.11）：

   | provider id | 环境变量 |
   | --- | --- |
   | `maestro-deepseek` | `MAESTRO_DEEPSEEK_API_KEY` |
   | `maestro-my_gateway` | `MAESTRO_MY_GATEWAY_API_KEY` |
   | `maestro-gpt4` | `MAESTRO_GPT_4_API_KEY` |
   | `maestro-claude-3-5` | `MAESTRO_CLAUDE_3_5_API_KEY` |

   注意转换规则不直观：slug 结尾的数字会被拆开（`gpt4` → `MAESTRO_GPT_4_API_KEY`），slug 里的 `_` 与 `-` 都变成 `_`。

插件既不伪造 `api_key` 字段，也不把凭证塞进 `custom_headers`——Zed 在取到凭证之前就会以 `NoApiKey` 直接失败，塞进去只是把错误推迟。

## 设计要点

- **命名空间 `maestro-`**。`settings.json` 是 Zed 与用户共享的配置文件（主题、键位、用户自己的 `openai_compatible` provider 都在里面，Zed 还会写入注释头），因此本插件不像 pi 插件那样整文件重写，而是只重建 `maestro-` 前缀的条目：先删除上一轮投影的条目再写入当前列表，Maestro 中已删除的 Provider 不会残留，用户自己配置的 provider 不受影响。前缀用 `-` 而非 `:`：Zed 用 provider id 推导取凭证的环境变量名，冒号会落进变量名而无法在 shell 中使用。
- **两个段落，一次投影写两处**。`language_models` 下 `openai_compatible` 与 `anthropic_compatible` 各自独立；Maestro 保证一个 Provider 只落在一个协议槽位，故按协议分流即可。段落原本不存在时只在确有条目时才创建，不制造空的 `openai_compatible: {}`；段落里只剩插件条目且被清空时，连同空壳一起移除。
- **`max_tokens` 固定为 131072**。该字段在 Zed 中是上下文窗口大小且必填，而 Maestro 的 Model 不携带该信息。取保守值 128k：声明偏小只会让 Zed 提前压缩上下文，不会引发服务端报错；可在 Zed 设置里自行改大。
- **不写 `capabilities`**，交给 Zed 的默认值：`tools: true`（工具可用）、`images: false`、`chat_completions: true`（走 `/chat/completions`，正合 `openai-completions` 协议）。要用视觉模型需在 Zed 设置里自行打开该模型的 `images`。
- **无损编辑**。经 [`jsonc-parser`](https://crates.io/crates/jsonc-parser) 的 CST 编辑，注释、缩进、空行与无关条目原样保留（`settings.json` 是 JSONC，会被用户写注释）。解析失败即报错放弃写入，不破坏既有配置；`language_models`、两个段落或文档根已存在但不是对象时同样报错放弃，绝不覆盖用户配置。
- **原子写入**。先写 `settings.json.tmp` 成功后再 rename 替换，I/O 错误不会留下半截的 `settings.json`。
- **平台范围**。manifest 的 `config_dir` 静态声明为 `$HOME/.config/zed`，只有 Linux 与 macOS 命中 Zed 的配置目录；Windows 上 Zed 用 `%APPDATA%\Zed`，`$XDG_CONFIG_HOME` 重定向同理不在支持范围内。
