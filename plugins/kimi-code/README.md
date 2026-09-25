# kimi-code

[Maestro](https://github.com/xezzon/agent-maestro) 的 Kimi Code 插件：把 Maestro 中录入的 Provider 投影进 `~/.kimi-code/config.toml`。

构建、调试与发布见仓库根 [README](../../README.md)。

## 投影规则

每个单协议 Provider 写入一条 `[providers."maestro:<slug>"]`，其每个 Model 写入一条 `[models."maestro:<slug>/<model-id>"]`：

| Maestro | Kimi Code `config.toml` |
| --- | --- |
| Provider slug `foo` | provider 名 `maestro:foo`，模型别名的 `provider` 字段 |
| 协议 `openai-completions` | `type = "openai"` |
| 协议 `anthropic-messages` | `type = "anthropic"` |
| Base URL | `base_url` |
| API Key | `api_key`（无凭证时省略，可自行补填或用 `[providers.*.env]` 兜底） |
| Model ID | `model` 字段，别名为 `maestro:<slug>/<model-id>` |
| Model 显示名 | `display_name`（空则省略） |
| — | `max_context_size = 131072`（固定默认值，见下） |

## 设计要点

- **只重建 `maestro:` 命名空间**。`config.toml` 是 Kimi Code 与用户共享的配置文件（`default_model`、`thinking`、`permission` 等都在里面），因此本插件不像 pi 插件那样整文件重写，而是以 [`toml_edit`](https://crates.io/crates/toml_edit) 无损编辑：先删除上一轮投影的 `maestro:` 条目再写入当前列表，注释、格式与其余条目原样保留。Maestro 中已删除的 Provider 不会残留。
- **`max_context_size` 固定为 131072**。该字段在 Kimi Code 中必填，而 Maestro 的 Model 不携带上下文长度信息。取保守值 128k：声明偏小只会让 Kimi Code 提前压缩上下文，不会引发服务端报错；如与实际不符，可在 `[models."<别名>".overrides]` 中覆盖。
- **不写 `default_model`**。投影后用 `/model` 选择即可。
- **不支持 `KIMI_CODE_HOME` 重定向**。插件可写的目录由 manifest 的 `config_dir` 静态声明为 `$HOME/.kimi-code`。
- 写入采用 tmp + rename 原子替换；文件解析失败时放弃写入并报错，不破坏既有配置。
