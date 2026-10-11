//! Kimi Code 插件：把 Maestro 录入的 Provider 投影为 `~/.kimi-code/config.toml` 中的
//! `[providers."maestro:<slug>"]` 与 `[models."maestro:<slug>/<model-id>"]` 条目。
//!
//! 与 pi 插件的整文件重写不同，`config.toml` 是 Kimi Code 与用户共享的配置文件
//! （`default_model`、`thinking`、`permission` 等都住在里面），因此本插件只重建
//! `maestro:` 命名空间下的条目：先删除该命名空间的旧条目再写入当前列表，
//! 其余内容经 `toml_edit` 无损保留（注释、格式、非插件条目原样不动）。
//! 命名空间约定参照 Kimi Code 自带的 `managed:kimi-code` provider 命名。
//!
//! `max_context_size` 是 Kimi Code 的必填字段，取 Model 的 `limit.context_window`；
//! 未设置时写固定默认值 [`DEFAULT_MAX_CONTEXT_SIZE`]；如与实际不符，用户可在
//! `[models."<alias>".overrides]` 中覆盖。
//!
//! Provider 可同时携带两种协议的端点与界面记录的投影选择；投影使用哪个端点由
//! 插件按 ADR 0016 的规则裁定：选择有效→用它，否则唯一端点，多端点且无有效选择
//! →优先 openai-completions。
//!
//! 依赖 [`maestro-plugin-sdk`]（`maestro:plugin` 合同的类型化绑定），实现其
//! `Guest` trait。宿主把 manifest 声明的 config_dir（即 `$HOME/.kimi-code`）预开放
//! 为 "/"，插件以相对路径读写。注意 Kimi Code 支持 `KIMI_CODE_HOME` 重定向数据
//! 目录，而插件 config_dir 由 manifest 静态声明，重定向场景不在支持范围内。

use std::fs;
use std::path::Path;

use maestro_plugin_sdk::{
    Endpoint, Guest, Level, Model, ModelCapability, Protocol, Provider, export, log,
};
use toml_edit::{Array, DocumentMut, Item, Table, value};

/// 插件写入的唯一文件（相对 config_dir）。
const CONFIG_FILE: &str = "config.toml";
/// 插件条目的命名空间前缀：provider 名与模型别名都以此为前缀，
/// 投影时据此识别并清理旧条目，避免误伤用户手工配置的条目。
const NAMESPACE: &str = "maestro:";
/// `max_context_size` 的兜底默认值：Kimi Code 要求该字段必填，而 Model 的
/// `limit.context_window` 可能缺省。取保守值（128k），声明偏小只会让 Kimi Code
/// 提前压缩上下文，不会引发服务端报错；用户可用 model overrides 覆盖。
const DEFAULT_MAX_CONTEXT_SIZE: i64 = 131072;

const KEY_TYPE: &str = "type";
const KEY_API_KEY: &str = "api_key";
const KEY_BASE_URL: &str = "base_url";
const KEY_CUSTOM_HEADERS: &str = "custom_headers";
const KEY_PROVIDER: &str = "provider";
const KEY_MODEL: &str = "model";
const KEY_MAX_CONTEXT_SIZE: &str = "max_context_size";
const KEY_MAX_INPUT_SIZE: &str = "max_input_size";
const KEY_MAX_OUTPUT_SIZE: &str = "max_output_size";
const KEY_CAPABILITIES: &str = "capabilities";
const KEY_DISPLAY_NAME: &str = "display_name";
const TABLE_PROVIDERS: &str = "providers";
const TABLE_MODELS: &str = "models";

struct KimiCodePlugin;

impl Guest for KimiCodePlugin {
    fn write_providers(providers: Vec<Provider>) -> Result<Vec<String>, String> {
        write_config(&providers)?;
        Ok(vec![CONFIG_FILE.to_owned()])
    }
}

fn write_config(providers: &[Provider]) -> Result<(), String> {
    let path = Path::new(CONFIG_FILE);
    let mut doc = match fs::read_to_string(path) {
        Ok(text) => text.parse::<DocumentMut>().map_err(|e| {
            format!("解析 config.toml 失败，为避免破坏既有配置已放弃写入\n原因：{e}")
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => DocumentMut::new(),
        Err(e) => return Err(format!("读取 config.toml 失败\n原因：{e}")),
    };

    // 端点选择在插件侧完成（ADR 0016）：先按规则为每个 Provider 选出投影端点。
    // 宿主保证列表内 Provider 至少有一个端点，取不到属防御分支，记日志跳过。
    let mut selected: Vec<(&Provider, &Endpoint)> = Vec::new();
    for provider in providers {
        match effective_endpoint(provider) {
            Some(endpoint) => selected.push((provider, endpoint)),
            None => log(
                Level::Warning,
                &format!(
                    "Provider \"{}\" has no usable endpoint; skipping",
                    provider.slug
                ),
            ),
        }
    }

    {
        let providers_table = ensure_table(&mut doc, TABLE_PROVIDERS)?;
        remove_namespace_entries(providers_table);
        for (provider, endpoint) in &selected {
            let provider_name = format!("{NAMESPACE}{}", provider.slug);
            providers_table.insert(&provider_name, provider_table(provider, endpoint));
        }
    }
    {
        let models_table = ensure_table(&mut doc, TABLE_MODELS)?;
        remove_namespace_entries(models_table);
        for (provider, _) in &selected {
            let provider_name = format!("{NAMESPACE}{}", provider.slug);
            for model in &provider.models {
                let alias = format!("{provider_name}/{}", model.id);
                models_table.insert(&alias, model_table(&provider_name, model));
            }
        }
    }

    // 原子写入：先写临时文件成功后再 rename，I/O 错误不会留下半截的 config.toml。
    let tmp = Path::new("config.toml.tmp");
    fs::write(tmp, doc.to_string())
        .map_err(|e| format!("写入 config.toml 临时文件失败\n原因：{e}"))?;
    fs::rename(tmp, path).map_err(|e| format!("替换 config.toml 失败\n原因：{e}"))?;

    Ok(())
}

/// 取出（或创建）`[providers]` / `[models]` 顶层表；
/// 已存在但不是标准表时放弃写入，避免破坏用户配置。
fn ensure_table<'a>(doc: &'a mut DocumentMut, name: &str) -> Result<&'a mut Table, String> {
    if !doc.contains_key(name) {
        doc.insert(name, Item::Table(Table::new()));
    } else if !doc[name].is_table() {
        return Err(format!(
            "[{name}] 已存在但不是标准表，为避免破坏既有配置已放弃写入"
        ));
    }
    Ok(doc[name].as_table_mut().expect("已确认是 table"))
}

/// 删除表内所有 `maestro:` 命名空间条目（上一轮投影的残留）。
fn remove_namespace_entries(table: &mut Table) {
    let stale: Vec<String> = table
        .iter()
        .filter(|(key, _)| key.starts_with(NAMESPACE))
        .map(|(key, _)| key.to_owned())
        .collect();
    for key in stale {
        table.remove(&key);
    }
}

/// 协议透传为 Kimi Code 的 provider type。
fn provider_type(protocol: &Protocol) -> &'static str {
    match protocol {
        Protocol::OpenaiCompletions => "openai",
        Protocol::AnthropicMessages => "anthropic",
    }
}

/// 选择投影端点（ADR 0016）：
/// 1. 界面所选协议确有端点 → 用它；
/// 2. 否则只剩一个端点 → 用它（覆盖旧配置无选择、选择失效两类情形）；
/// 3. 否则多端点且无有效选择 → 取固定顺序里的首选 openai-completions。
fn effective_endpoint(provider: &Provider) -> Option<&Endpoint> {
    if let Some(selected) = &provider.selected_protocol
        && let Some(endpoint) = provider
            .endpoints
            .iter()
            .find(|endpoint| &endpoint.protocol == selected)
    {
        return Some(endpoint);
    }
    if provider.endpoints.len() == 1 {
        return provider.endpoints.first();
    }
    provider
        .endpoints
        .iter()
        .find(|endpoint| matches!(endpoint.protocol, Protocol::OpenaiCompletions))
        .or_else(|| provider.endpoints.first())
}

/// 单个 Provider 的 provider 条目：类型与 Base URL 取自 [`effective_endpoint`]
/// 选出的端点，API Key 与自定义 header 为 Provider 级。
fn provider_table(provider: &Provider, endpoint: &Endpoint) -> Item {
    let mut entry = Table::new();
    entry.insert(KEY_TYPE, value(provider_type(&endpoint.protocol)));
    entry.insert(KEY_BASE_URL, value(endpoint.base_url.as_str()));
    // 无凭证时宿主传 None，本地网关模型在 Kimi Code 中仍可见；
    // 用户可自行补 api_key 或 env 兜底。
    if let Some(api_key) = &provider.api_key {
        entry.insert(KEY_API_KEY, value(api_key.as_str()));
    }
    // 自定义 header：没有时省略整段，不写空表。宿主已按名排序，此处不再重排。
    if !provider.custom_header.is_empty() {
        let mut headers = Table::new();
        for header in &provider.custom_header {
            headers.insert(&header.name, value(header.value.as_str()));
        }
        entry.insert(KEY_CUSTOM_HEADERS, Item::Table(headers));
    }
    Item::Table(entry)
}

/// 单个 Model 的 model 条目：token 上限与能力均有值才写，未设置一律省略。
/// `max_context_size` 必填，缺省时回落到 [`DEFAULT_MAX_CONTEXT_SIZE`]。
fn model_table(provider_name: &str, model: &Model) -> Item {
    let mut entry = Table::new();
    entry.insert(KEY_PROVIDER, value(provider_name));
    entry.insert(KEY_MODEL, value(model.id.as_str()));
    entry.insert(
        KEY_MAX_CONTEXT_SIZE,
        value(
            positive(model.limit.context_window)
                .map(i64::from)
                .unwrap_or(DEFAULT_MAX_CONTEXT_SIZE),
        ),
    );
    if let Some(max_input) = positive(model.limit.max_input) {
        entry.insert(KEY_MAX_INPUT_SIZE, value(i64::from(max_input)));
    }
    if let Some(max_output) = positive(model.limit.max_output) {
        entry.insert(KEY_MAX_OUTPUT_SIZE, value(i64::from(max_output)));
    }
    let capabilities = capability_tags(&model.capabilities);
    if !capabilities.is_empty() {
        let mut array = Array::new();
        for tag in capabilities {
            array.push(tag);
        }
        entry.insert(KEY_CAPABILITIES, value(array));
    }
    if let Some(name) = &model.display_name
        && !name.is_empty()
    {
        entry.insert(KEY_DISPLAY_NAME, value(name.as_str()));
    }
    Item::Table(entry)
}

/// Kimi Code 的能力标签（`capabilities` 数组）。按合同声明序输出，未设置的能力不写。
fn capability_tags(capabilities: &[ModelCapability]) -> Vec<&'static str> {
    let mut tags = Vec::new();
    if capabilities.contains(&ModelCapability::ToolUse) {
        tags.push("tool_use");
    }
    if capabilities.contains(&ModelCapability::ImageIn) {
        tags.push("image_in");
    }
    if capabilities.contains(&ModelCapability::Thinking) {
        tags.push("thinking");
    }
    tags
}

/// 声明式上限只在为正时才投影：0 是唯一可能的非正取值（`u32` 承载，
/// 负值在记录层即被类型拒绝），跳过且不记日志。
fn positive(limit: Option<u32>) -> Option<u32> {
    limit.filter(|limit| *limit > 0)
}

export!(KimiCodePlugin);

#[cfg(test)]
mod tests {
    use super::*;
    use maestro_plugin_sdk::{CustomHeader, ModelLimit};

    const NO_LIMIT: ModelLimit = ModelLimit {
        context_window: None,
        max_input: None,
        max_output: None,
    };

    fn endpoint() -> Endpoint {
        Endpoint {
            protocol: Protocol::OpenaiCompletions,
            base_url: "https://gateway.example.com/v1".to_owned(),
        }
    }

    fn provider_with_headers(headers: &[(&str, &str)]) -> Provider {
        Provider {
            slug: "foo".to_owned(),
            endpoints: vec![endpoint()],
            selected_protocol: None,
            api_key: None,
            custom_header: headers
                .iter()
                .map(|(name, value)| CustomHeader {
                    name: (*name).to_owned(),
                    value: (*value).to_owned(),
                })
                .collect(),
            models: Vec::new(),
        }
    }

    fn model_with(id: &str, limit: ModelLimit, capabilities: Vec<ModelCapability>) -> Model {
        Model {
            id: id.to_owned(),
            display_name: None,
            limit,
            capabilities,
        }
    }

    /// 把构造出的条目挂到键下渲染成 TOML，再解析回来断言（兼作语法往返校验）。
    fn render(item: Item, key: &str) -> DocumentMut {
        let mut doc = DocumentMut::new();
        doc.insert(key, item);
        doc.to_string()
            .parse::<DocumentMut>()
            .expect("渲染结果应是合法 TOML")
    }

    #[test]
    fn projects_custom_headers() {
        let provider = provider_with_headers(&[("X-A", "1"), ("Authorization", "Bearer t")]);
        let doc = render(provider_table(&provider, &endpoint()), "p");
        assert_eq!(doc["p"]["type"].as_str(), Some("openai"));
        assert_eq!(doc["p"]["custom_headers"]["X-A"].as_str(), Some("1"));
        assert_eq!(
            doc["p"]["custom_headers"]["Authorization"].as_str(),
            Some("Bearer t")
        );
    }

    #[test]
    fn omits_custom_headers_when_absent() {
        let doc = render(
            provider_table(&provider_with_headers(&[]), &endpoint()),
            "p",
        );
        assert!(!doc.to_string().contains("custom_headers"));
    }

    #[test]
    fn projects_model_limits() {
        let model = model_with(
            "gpt-5",
            ModelLimit {
                context_window: Some(200_000),
                max_input: Some(100_000),
                max_output: Some(8_192),
            },
            Vec::new(),
        );
        let doc = render(model_table("maestro:foo", &model), "m");
        assert_eq!(doc["m"]["max_context_size"].as_integer(), Some(200_000));
        assert_eq!(doc["m"]["max_input_size"].as_integer(), Some(100_000));
        assert_eq!(doc["m"]["max_output_size"].as_integer(), Some(8_192));
    }

    #[test]
    fn falls_back_to_default_context_size_and_omits_optional_limits() {
        let model = model_with("m", NO_LIMIT, Vec::new());
        let doc = render(model_table("maestro:foo", &model), "m");
        assert_eq!(
            doc["m"]["max_context_size"].as_integer(),
            Some(DEFAULT_MAX_CONTEXT_SIZE)
        );
        let text = doc.to_string();
        assert!(!text.contains("max_input_size"));
        assert!(!text.contains("max_output_size"));
    }

    #[test]
    fn projects_model_capabilities() {
        let model = model_with(
            "m",
            NO_LIMIT,
            vec![
                ModelCapability::ToolUse,
                ModelCapability::ImageIn,
                ModelCapability::Thinking,
            ],
        );
        let doc = render(model_table("maestro:foo", &model), "m");
        let tags: Vec<&str> = doc["m"]["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|value| value.as_str())
            .collect();
        assert_eq!(tags, ["tool_use", "image_in", "thinking"]);
    }

    #[test]
    fn omits_capabilities_when_unset() {
        let model = model_with("m", NO_LIMIT, Vec::new());
        let doc = render(model_table("maestro:foo", &model), "m");
        assert!(!doc.to_string().contains("capabilities"));
    }
}
