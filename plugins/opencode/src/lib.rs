//! OpenCode 插件：把 Maestro 录入的 Provider 投影为
//! `~/.config/opencode/opencode.json` 中的 `providers["maestro-<slug>"]` 条目，
//! 使录入的 Provider / 模型直接出现在 OpenCode 的 `/models` 选择器中。
//!
//! `opencode.json` 是 OpenCode 与用户共享的全局配置（`permission`、`mcp`、
//! `agents` 等都住在里面），且官方支持 JSONC（注释与尾逗号）。与 kimi-code 用
//! `toml_edit` 的做法对齐，本插件经 [`jsonc_parser`] 的 CST 无损编辑：只重建
//! `maestro-` 前缀的 provider 条目，注释、格式与其余内容原样保留。
//!
//! OpenCode 2 把自定义 provider 放在顶层 `providers`（v1 的 `provider` 已废弃）；
//! 一个 provider 挂在一个 AI SDK 包上，一种协议一个 provider。provider id 与
//! models.dev 内置目录共享命名空间，故加 `maestro-` 前缀防撞名，该前缀同时充当
//! 清理旧条目的匹配依据（对齐 kimi-code 的 `maestro:` 命名空间）。
//!
//! Provider 可同时携带两种协议的端点与界面记录的投影选择；投影使用哪个端点由
//! 插件按 ADR 0016 的规则裁定，再据选中端点的协议挂对应的 AI SDK 包。
//!
//! 依赖 [`maestro-plugin-sdk`]（`maestro:plugin` 合同的类型化绑定），实现其
//! `Guest` trait。宿主把 manifest 声明的 config_dir（即 `$HOME/.config/opencode`）
//! 预开放为 "/"，插件以相对路径读写。

use std::fs;
use std::path::Path;

use jsonc_parser::ParseOptions;
use jsonc_parser::cst::{CstInputValue, CstObject, CstRootNode};
use maestro_plugin_sdk::{
    CustomHeader, Endpoint, Guest, Level, Model, ModelCapability, Protocol, Provider, export, log,
};
use url::Url;

/// 插件写入的唯一文件（相对 config_dir）。
const CONFIG_FILE: &str = "opencode.json";
/// 新建文件时写入的 schema 声明；已存在的文件不做改动。
const SCHEMA_KEY: &str = "$schema";
const SCHEMA_URL: &str = "https://opencode.ai/config.json";
/// 插件条目的命名空间前缀：provider id 都以此为前缀，投影时据此识别并清理
/// 旧条目，避免误伤用户手工配置的条目（OpenCode 的 provider id 与 models.dev
/// 内置目录共享命名空间，`anthropic`、`openai` 等是内置 id）。
const NAMESPACE: &str = "maestro-";
/// OpenCode 2 的 provider 表（v1 的 `provider` 已废弃）。
const KEY_PROVIDERS: &str = "providers";
/// 已废弃的 v1 provider 表：仅用于清理本插件早期版本写入的残留条目。
const LEGACY_KEY_PROVIDER: &str = "provider";
const KEY_PACKAGE: &str = "package";
const KEY_SETTINGS: &str = "settings";
const KEY_BASE_URL: &str = "baseURL";
const KEY_API_KEY: &str = "apiKey";
const KEY_MODELS: &str = "models";
const KEY_NAME: &str = "name";
const KEY_HEADERS: &str = "headers";
const KEY_CAPABILITIES: &str = "capabilities";
const KEY_TOOLS: &str = "tools";
const KEY_INPUT: &str = "input";
const KEY_LIMIT: &str = "limit";
const KEY_CONTEXT: &str = "context";
const KEY_OUTPUT: &str = "output";
/// OpenCode 的 `package` 用 `aisdk:` 前缀标记 AI SDK 包（对齐 `Provider.aisdk`；
/// v1→v2 迁移的 `packageSpecifier` 即把 npm 名编码成这种形式）。
const AISDK_PREFIX: &str = "aisdk:";
const NPM_OPENAI_COMPATIBLE: &str = "@ai-sdk/openai-compatible";
const NPM_ANTHROPIC: &str = "@ai-sdk/anthropic";
/// Anthropic Messages 的 API 版本路径段：运行时在 baseURL 后追加 `/messages`，
/// 版本前缀因而必须由 baseURL 自身带出（见 [`normalize_anthropic_base_url`]）。
const VERSION_SEGMENT: &str = "v1";
/// 与 [`VERSION_SEGMENT`] 对应的路径后缀，用于判断末段是否已是版本段。
const VERSION_SUFFIX: &str = "/v1";
/// 解析选项：只放行 OpenCode 官方支持的 JSONC 扩展（注释与尾逗号），其余宽松
/// 语法（缺逗号、单引号、未加引号的键等）一律拒绝，避免写回 OpenCode 读不回的配置。
const PARSE_OPTIONS: ParseOptions = ParseOptions {
    allow_comments: true,
    allow_trailing_commas: true,
    allow_loose_object_property_names: false,
    allow_missing_commas: false,
    allow_single_quoted_strings: false,
    allow_hexadecimal_numbers: false,
    allow_unary_plus_numbers: false,
    allow_bare_decimal_point_numbers: false,
    allow_non_finite_numbers: false,
    allow_extended_string_escapes: false,
};

struct OpenCodePlugin;

impl Guest for OpenCodePlugin {
    fn write_providers(providers: Vec<Provider>) -> Result<Vec<String>, String> {
        write_config(&providers)?;
        Ok(vec![CONFIG_FILE.to_owned()])
    }
}

/// 读取（或新建）`opencode.json` 并写入投影结果。
/// 落盘采用 tmp + rename 原子替换：先写临时文件成功后再 rename，
/// I/O 错误不会留下半截的 opencode.json。
fn write_config(providers: &[Provider]) -> Result<(), String> {
    let path = Path::new(CONFIG_FILE);
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("读取 {CONFIG_FILE} 失败\n原因：{e}")),
    };

    let new_text = edit_config(&text, providers)?;

    // 原子写入：先写临时文件成功后再 rename，I/O 错误不会留下半截的 opencode.json。
    let tmp = Path::new("opencode.json.tmp");
    fs::write(tmp, new_text).map_err(|e| format!("写入 {CONFIG_FILE} 临时文件失败\n原因：{e}"))?;
    fs::rename(tmp, path).map_err(|e| format!("替换 {CONFIG_FILE} 失败\n原因：{e}"))?;

    Ok(())
}

/// 把投影应用到配置文本：解析（容忍注释与尾逗号）→ 删除 `maestro-` 前缀旧条目
/// → 写入当前列表。文件不存在或为空时以新文档为种子（带官方 schema 声明）；
/// 解析失败或结构不符合预期时返回 Err，调用方保持原文件不动。
fn edit_config(text: &str, providers: &[Provider]) -> Result<String, String> {
    let root = match text.trim().is_empty() {
        true => new_document()?,
        false => CstRootNode::parse(text, &PARSE_OPTIONS).map_err(|e| {
            format!("解析 {CONFIG_FILE} 失败，为避免破坏既有配置已放弃写入\n原因：{e}")
        })?,
    };
    let root_obj = root
        .object_value()
        .ok_or_else(|| "根节点不是对象，为避免破坏既有配置已放弃写入".to_owned())?;

    // 先清理早期版本遗留在已废弃 `provider` 表里的条目，再重建 `providers`。
    remove_legacy_namespace(&root_obj);

    let provider_obj = match root_obj.get(KEY_PROVIDERS) {
        Some(prop) => prop
            .value()
            .and_then(|v| v.as_object())
            .ok_or_else(|| "providers 已存在但不是对象，为避免破坏既有配置已放弃写入".to_owned())?,
        None => root_obj
            .append(KEY_PROVIDERS, CstInputValue::Object(vec![]))
            .object_value_or_set(),
    };

    remove_namespace_entries(&provider_obj);
    for provider in providers {
        // 端点选择在插件侧完成（ADR 0016）：宿主保证列表内 Provider 至少有一个
        // 端点，取不到属防御分支，记日志跳过。
        let Some(endpoint) = effective_endpoint(provider) else {
            log(
                Level::Warning,
                &format!(
                    "Provider \"{}\" has no usable endpoint; skipping",
                    provider.slug
                ),
            );
            continue;
        };
        provider_obj.append(
            &provider_id(&provider.slug),
            provider_entry(provider, endpoint),
        );
    }

    Ok(root.to_string())
}

/// 以带官方 schema 声明的空对象为种子，构造新配置文档。
fn new_document() -> Result<CstRootNode, String> {
    let root = CstRootNode::parse("{}", &PARSE_OPTIONS)
        .map_err(|e| format!("初始化新 {CONFIG_FILE} 失败\n原因：{e}"))?;
    root.object_value_or_set()
        .append(SCHEMA_KEY, SCHEMA_URL.into());
    Ok(root)
}

/// OpenCode 的 provider id 一律为 `maestro-<slug>`：slug 在 Maestro 中唯一且宿主
/// 每个 slug 只下发一条 Provider，映射天然单射；插件为每条 Provider 选出唯一端点
/// （见 [`effective_endpoint`]），只挂该端点协议对应的 npm 包（见 [`provider_npm`]）。
fn provider_id(slug: &str) -> String {
    format!("{NAMESPACE}{slug}")
}

/// 删除表内所有 `maestro-` 前缀条目（上一轮投影的残留）。
fn remove_namespace_entries(provider_obj: &CstObject) {
    for prop in provider_obj.properties() {
        if prop
            .decoded_name()
            .is_some_and(|name| name.starts_with(NAMESPACE))
        {
            prop.remove();
        }
    }
}

/// 清理已废弃的 v1 `provider` 表里本插件早期版本写入的 `maestro-` 条目：
/// OpenCode 2 会把 `provider` 迁移进 `providers`，若不同步清理就会与新条目重复。
/// 整表被清空时一并移除该字段，只保留用户手写的条目。
fn remove_legacy_namespace(root_obj: &CstObject) {
    let Some(prop) = root_obj.get(LEGACY_KEY_PROVIDER) else {
        return;
    };
    let Some(legacy) = prop.value().and_then(|value| value.as_object()) else {
        return;
    };
    remove_namespace_entries(&legacy);
    if legacy.properties().is_empty() {
        prop.remove();
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

/// 协议透传为 OpenCode 挂载的 AI SDK 包名。
fn provider_npm(protocol: &Protocol) -> &'static str {
    match protocol {
        Protocol::OpenaiCompletions => NPM_OPENAI_COMPATIBLE,
        Protocol::AnthropicMessages => NPM_ANTHROPIC,
    }
}

/// Anthropic Messages 的请求路径由运行时拼成 `${baseURL}/messages`：OpenCode 2
/// 的原生路由（`Endpoint.render`）与 `@ai-sdk/anthropic` 的 `buildRequestUrl`
/// 都只在 baseURL 后追加 `/messages`，且仅当 baseURL 恰为官方主机
/// `https://api.anthropic.com` 时才自动补 `/v1`。自定义网关若只填到
/// `https://gateway.example.com`，请求会落到 `/messages` 而非 `/v1/messages`
/// 并返回 404，因此投影时补齐版本前缀：已有 `/v1` 原样保留，否则追加。
fn normalize_anthropic_base_url(base_url: &str) -> String {
    // 按 URL 结构改路径而非拼接字符串：先解析成 [`Url`]，只动 path 的末段，
    // scheme / host / query / fragment 各归各位，不受彼此干扰。
    let Ok(mut url) = Url::parse(base_url) else {
        return base_url.to_owned();
    };
    // 段与段之间恒有 `/`，故按路径后缀判断末段：`/v1` 与 `/anthropic/v1` 都命中，
    // `/v1beta` 这类只是前缀相同的段不会误判。尾斜杠不是路径段，先排除。
    let versioned = url.path().trim_end_matches('/').ends_with(VERSION_SUFFIX);
    // `path_segments_mut` 对 cannot-be-a-base 的 URL（如 `mailto:`）返回 Err，
    // 这类地址没有可改写的路径，原样透传；`PathSegmentsMut` 带 `Drop`，借用活到
    // 块末，故改写单独成块。
    {
        let Ok(mut segments) = url.path_segments_mut() else {
            return base_url.to_owned();
        };
        // 已带版本前缀即原样返回：用户的 URL 写法（大小写、编码、尾斜杠）一律不动。
        if versioned {
            return base_url.to_owned();
        }
        // 追加前摘掉尾斜杠，否则会写成 `/anthropic//v1`。
        segments.pop_if_empty();
        segments.push(VERSION_SEGMENT);
    }
    url.into()
}

/// 单个 Provider 的 OpenCode 条目：`package`（AI SDK 包）、`settings`（`baseURL`
/// 与可选的 `apiKey`）、可选的 `headers` 及 `models`。`package` 与 `baseURL` 取自
/// [`effective_endpoint`] 选出的端点，`apiKey` 与 `headers` 为 Provider 级。
/// Anthropic 端点补 `/v1`（见 [`normalize_anthropic_base_url`]）；OpenAI 兼容端点
/// 保持原样，端点路径语义归用户。
fn provider_entry(provider: &Provider, endpoint: &Endpoint) -> CstInputValue {
    let base_url = match &endpoint.protocol {
        Protocol::AnthropicMessages => normalize_anthropic_base_url(&endpoint.base_url),
        Protocol::OpenaiCompletions => endpoint.base_url.clone(),
    };
    let mut settings = vec![(
        KEY_BASE_URL.to_owned(),
        CstInputValue::from(base_url.as_str()),
    )];
    // 无凭证时宿主传 None，本地网关模型在 OpenCode 中仍可见；
    // 用户可自行补 apiKey 或用 {env:VAR} / {file:path} 插值兜底。
    if let Some(api_key) = &provider.api_key {
        settings.push((
            KEY_API_KEY.to_owned(),
            CstInputValue::from(api_key.as_str()),
        ));
    }

    let models = provider
        .models
        .iter()
        .map(|model| (model.id.clone(), model_entry(model)))
        .collect();

    let package = format!("{AISDK_PREFIX}{}", provider_npm(&endpoint.protocol));
    let mut entry = vec![
        (
            KEY_PACKAGE.to_owned(),
            CstInputValue::from(package.as_str()),
        ),
        (KEY_SETTINGS.to_owned(), CstInputValue::Object(settings)),
    ];
    // 没有自定义 header 时省略整段，不写空对象。
    if !provider.custom_header.is_empty() {
        entry.push((
            KEY_HEADERS.to_owned(),
            headers_value(&provider.custom_header),
        ));
    }
    entry.push((KEY_MODELS.to_owned(), CstInputValue::Object(models)));
    CstInputValue::Object(entry)
}

/// 自定义 header：`name` → `value` 对象。宿主已按名排序，此处不再重排。
fn headers_value(headers: &[CustomHeader]) -> CstInputValue {
    CstInputValue::Object(
        headers
            .iter()
            .map(|header| {
                (
                    header.name.clone(),
                    CstInputValue::from(header.value.as_str()),
                )
            })
            .collect(),
    )
}

/// 单个 Model 的条目：显示名、能力与 token 上限均有值才写，未设置一律省略。
fn model_entry(model: &Model) -> CstInputValue {
    let mut entry = Vec::new();
    // 合同未携带 Model 展示名时省略 name 字段。
    if let Some(name) = &model.display_name
        && !name.is_empty()
    {
        entry.push((KEY_NAME.to_owned(), CstInputValue::from(name.as_str())));
    }
    if let Some(capabilities) = model_capabilities(model) {
        entry.push((KEY_CAPABILITIES.to_owned(), capabilities));
    }
    if let Some(limit) = model_limit(model) {
        entry.push((KEY_LIMIT.to_owned(), limit));
    }
    CstInputValue::Object(entry)
}

/// Model 的能力声明（`capabilities`）。OpenCode 2 用 `capabilities.tools` 表示工具
/// 调用、`capabilities.input` 表示可接收的模态；能力只在声明为正时写出，未设置一律
/// 省略（沿用「不写 false / 空数组」的约定）。OpenCode 2 没有模型级 reasoning 开关，
/// 故 Maestro 的 `thinking` 不再投影。
fn model_capabilities(model: &Model) -> Option<CstInputValue> {
    let mut fields = Vec::new();
    if model.capabilities.contains(&ModelCapability::ToolUse) {
        fields.push((KEY_TOOLS.to_owned(), true.into()));
    }
    // 能收图即两种模态都写：OpenCode 的 capabilities.input 是完整列表，
    // 没有「只收图不收字」的模型。
    if model.capabilities.contains(&ModelCapability::ImageIn) {
        fields.push((KEY_INPUT.to_owned(), vec!["text", "image"].into()));
    }
    (!fields.is_empty()).then(|| CstInputValue::Object(fields))
}

/// Model 的 token 上限。OpenCode 2 的 `limit` 三个字段都是可选的，故每个声明的
/// 正值各自写出，只按有值与否省略。
fn model_limit(model: &Model) -> Option<CstInputValue> {
    let mut fields = Vec::new();
    if let Some(context) = positive(model.limit.context_window) {
        fields.push((KEY_CONTEXT.to_owned(), context.into()));
    }
    if let Some(input) = positive(model.limit.max_input) {
        fields.push((KEY_INPUT.to_owned(), input.into()));
    }
    if let Some(output) = positive(model.limit.max_output) {
        fields.push((KEY_OUTPUT.to_owned(), output.into()));
    }
    (!fields.is_empty()).then(|| CstInputValue::Object(fields))
}

/// 声明式上限只在为正时才投影：0 是唯一可能的非正取值（`u32` 承载，
/// 负值在记录层即被类型拒绝），跳过且不记日志。
fn positive(limit: Option<u32>) -> Option<u32> {
    limit.filter(|limit| *limit > 0)
}

export!(OpenCodePlugin);

#[cfg(test)]
mod tests {
    use super::*;
    use maestro_plugin_sdk::{CustomHeader, Endpoint, Model, ModelCapability, ModelLimit};

    /// `package` 字段的期望字面值：v1 的 `npm` 经 OpenCode 迁移后带 `aisdk:` 前缀。
    const PACKAGE_OPENAI: &str = "aisdk:@ai-sdk/openai-compatible";
    const PACKAGE_ANTHROPIC: &str = "aisdk:@ai-sdk/anthropic";

    /// 投影结果带注释（JSONC），用 jsonc-parser 解析成 serde_json::Value 再断言，
    /// 同时兼作输出可被 JSONC 解析器读回的往返校验。
    fn parse_jsonc(text: &str) -> serde_json::Value {
        jsonc_parser::parse_to_serde_value::<serde_json::Value>(text, &Default::default()).unwrap()
    }

    /// 构造单端点测试用 Provider，逐字段给定投影输入的各项值。
    fn provider(
        slug: &str,
        protocol: Protocol,
        base_url: &str,
        api_key: Option<&str>,
        models: &[(&str, Option<&str>)],
    ) -> Provider {
        Provider {
            slug: slug.to_owned(),
            endpoints: vec![Endpoint {
                protocol,
                base_url: base_url.to_owned(),
            }],
            selected_protocol: None,
            api_key: api_key.map(str::to_owned),
            custom_header: Vec::new(),
            models: models
                .iter()
                .map(|(id, name)| Model {
                    id: id.to_string(),
                    display_name: name.map(str::to_owned),
                    limit: ModelLimit {
                        context_window: None,
                        max_input: None,
                        max_output: None,
                    },
                    capabilities: Vec::new(),
                })
                .collect(),
        }
    }

    /// 构造双端点测试用 Provider（openai 在前，与宿主收集顺序一致），
    /// 并可指定界面记录的投影选择。
    fn dual_endpoint(
        slug: &str,
        openai_url: &str,
        anthropic_url: &str,
        selected: Option<Protocol>,
    ) -> Provider {
        Provider {
            slug: slug.to_owned(),
            endpoints: vec![
                Endpoint {
                    protocol: Protocol::OpenaiCompletions,
                    base_url: openai_url.to_owned(),
                },
                Endpoint {
                    protocol: Protocol::AnthropicMessages,
                    base_url: anthropic_url.to_owned(),
                },
            ],
            selected_protocol: selected,
            api_key: None,
            custom_header: Vec::new(),
            models: Vec::new(),
        }
    }

    /// 构造带固定 Base URL 与 API Key 的 openai-completions 测试 Provider。
    fn openai(slug: &str, models: &[(&str, Option<&str>)]) -> Provider {
        provider(
            slug,
            Protocol::OpenaiCompletions,
            "https://gateway.example.com/v1",
            Some("sk-..."),
            models,
        )
    }

    #[test]
    fn creates_document_with_schema_when_file_missing() {
        let out = edit_config("", &[openai("foo", &[("gpt-5", Some("GPT-5"))])]).unwrap();
        let doc = parse_jsonc(&out);
        assert_eq!(doc[SCHEMA_KEY], SCHEMA_URL);
        assert_eq!(
            doc[KEY_PROVIDERS]["maestro-foo"][KEY_PACKAGE].as_str(),
            Some(PACKAGE_OPENAI)
        );
        assert_eq!(
            doc[KEY_PROVIDERS]["maestro-foo"][KEY_SETTINGS][KEY_BASE_URL],
            "https://gateway.example.com/v1"
        );
        assert_eq!(
            doc[KEY_PROVIDERS]["maestro-foo"][KEY_SETTINGS][KEY_API_KEY],
            "sk-..."
        );
        assert_eq!(
            doc[KEY_PROVIDERS]["maestro-foo"][KEY_MODELS]["gpt-5"][KEY_NAME],
            "GPT-5"
        );
    }

    #[test]
    fn preserves_user_config_and_comments() {
        let input = r#"{
  // model config
  "model": "some/model",
  "permissions": [],
  "providers": {
    "my-own": {
      "package": "@ai-sdk/openai-compatible", // hand-written entry
      "settings": { "baseURL": "https://my.example.com/v1", },
    },
  },
}
"#;
        let out = edit_config(input, &[openai("foo", &[("gpt-5", None)])]).unwrap();
        // 投影本身是合法 JSON，且用户内容原样保留。
        let doc = parse_jsonc(&out);
        assert_eq!(doc["model"], "some/model");
        assert_eq!(
            doc[KEY_PROVIDERS]["my-own"][KEY_SETTINGS][KEY_BASE_URL],
            "https://my.example.com/v1"
        );
        assert!(out.contains("// model config"));
        assert!(out.contains("// hand-written entry"));
        assert_eq!(
            doc[KEY_PROVIDERS]["maestro-foo"][KEY_PACKAGE].as_str(),
            Some(PACKAGE_OPENAI)
        );
    }

    #[test]
    fn reprojection_replaces_entries_and_removes_stale_ones() {
        let input = r#"{
  "providers": {
    "maestro-old": { "package": "@ai-sdk/openai-compatible", "settings": { "baseURL": "https://old.example.com/v1" }, "models": {} },
    "my-own": { "package": "@ai-sdk/openai-compatible", "settings": { "baseURL": "https://my.example.com/v1" } }
  }
}"#;
        let out = edit_config(input, &[openai("foo", &[("gpt-5", None)])]).unwrap();
        let doc = parse_jsonc(&out);
        assert!(
            doc[KEY_PROVIDERS].get("maestro-old").is_none(),
            "旧条目应被清理"
        );
        assert!(
            doc[KEY_PROVIDERS].get("my-own").is_some(),
            "用户条目不应误伤"
        );
        assert_eq!(
            doc[KEY_PROVIDERS]["maestro-foo"][KEY_SETTINGS][KEY_BASE_URL],
            "https://gateway.example.com/v1"
        );
    }

    #[test]
    fn cleans_stale_entries_from_legacy_provider_table() {
        // 早期版本写进已废弃 `provider` 表的条目必须清理，否则 OpenCode 迁移后会重复。
        let input = r#"{
  "provider": {
    "maestro-old": { "npm": "@ai-sdk/openai-compatible", "options": { "baseURL": "https://old.example.com/v1" } },
    "my-own": { "npm": "@ai-sdk/openai-compatible", "options": { "baseURL": "https://my.example.com/v1" } }
  }
}"#;
        let out = edit_config(input, &[openai("foo", &[("gpt-5", None)])]).unwrap();
        let doc = parse_jsonc(&out);
        assert!(
            doc[LEGACY_KEY_PROVIDER].get("maestro-old").is_none(),
            "v1 残留应被清理"
        );
        assert!(
            doc[LEGACY_KEY_PROVIDER].get("my-own").is_some(),
            "用户手写条目不应误伤"
        );
        assert_eq!(
            doc[KEY_PROVIDERS]["maestro-foo"][KEY_PACKAGE].as_str(),
            Some(PACKAGE_OPENAI)
        );
    }

    #[test]
    fn removes_legacy_provider_table_once_emptied() {
        let input = r#"{ "provider": { "maestro-old": { "npm": "@ai-sdk/openai-compatible" } } }"#;
        let out = edit_config(input, &[]).unwrap();
        let doc = parse_jsonc(&out);
        assert!(
            doc.get(LEGACY_KEY_PROVIDER).is_none(),
            "清空后的 v1 provider 表应整体移除"
        );
    }

    #[test]
    fn anthropic_protocol_maps_package_and_versioned_base_url() {
        let p = provider(
            "foo",
            Protocol::AnthropicMessages,
            "https://gw.example.com",
            None,
            &[("claude-5", Some("Claude 5"))],
        );
        let out = edit_config("{}", &[p]).unwrap();
        let doc = parse_jsonc(&out);
        assert_eq!(
            doc[KEY_PROVIDERS]["maestro-foo"][KEY_PACKAGE].as_str(),
            Some(PACKAGE_ANTHROPIC)
        );
        assert_eq!(
            doc[KEY_PROVIDERS]["maestro-foo"][KEY_SETTINGS][KEY_BASE_URL],
            "https://gw.example.com/v1",
            "Anthropic 自定义 baseURL 必须补 /v1"
        );
        assert_eq!(
            doc[KEY_PROVIDERS]["maestro-foo"][KEY_MODELS]["claude-5"][KEY_NAME],
            "Claude 5"
        );
        assert!(
            doc[KEY_PROVIDERS]["maestro-foo"][KEY_SETTINGS]
                .get(KEY_API_KEY)
                .is_none()
        );
    }

    #[test]
    fn anthropic_base_url_normalization_is_idempotent() {
        for (input, expected) in [
            ("https://gw.example.com", "https://gw.example.com/v1"),
            ("https://gw.example.com/", "https://gw.example.com/v1"),
            (
                "https://gw.example.com/anthropic",
                "https://gw.example.com/anthropic/v1",
            ),
            (
                "https://gw.example.com/anthropic/",
                "https://gw.example.com/anthropic/v1",
            ),
            ("https://gw.example.com/v1", "https://gw.example.com/v1"),
            // 已带版本前缀即原样返回：尾斜杠等用户写法不被插件改写。
            ("https://gw.example.com/v1/", "https://gw.example.com/v1/"),
            ("http://127.0.0.1:8080", "http://127.0.0.1:8080/v1"),
            (
                "https://gw.example.com?x=1",
                "https://gw.example.com/v1?x=1",
            ),
            (
                "https://gw.example.com/anthropic?x=1#frag",
                "https://gw.example.com/anthropic/v1?x=1#frag",
            ),
            // 解析不了的地址不猜、不改写，原样交给 OpenCode 报错。
            ("not a url", "not a url"),
        ] {
            let p = provider("foo", Protocol::AnthropicMessages, input, None, &[]);
            let out = edit_config("{}", &[p]).unwrap();
            let doc = parse_jsonc(&out);
            assert_eq!(
                doc[KEY_PROVIDERS]["maestro-foo"][KEY_SETTINGS][KEY_BASE_URL], expected,
                "输入 {input}"
            );
        }
    }

    #[test]
    fn leaves_openai_base_url_untouched() {
        // OpenAI 兼容端点的路径语义归用户，插件不改写（与 sourcegit 的约定一致）。
        let p = provider(
            "foo",
            Protocol::OpenaiCompletions,
            "https://gw.example.com",
            None,
            &[],
        );
        let out = edit_config("{}", &[p]).unwrap();
        let doc = parse_jsonc(&out);
        assert_eq!(
            doc[KEY_PROVIDERS]["maestro-foo"][KEY_SETTINGS][KEY_BASE_URL],
            "https://gw.example.com"
        );
    }

    #[test]
    fn omits_optional_fields() {
        let p = provider(
            "foo",
            Protocol::OpenaiCompletions,
            "https://gw.example.com/v1",
            None,
            &[("m1", None), ("m2", Some(""))],
        );
        let out = edit_config("{}", &[p]).unwrap();
        let doc = parse_jsonc(&out);
        let entry = &doc[KEY_PROVIDERS]["maestro-foo"];
        assert!(
            entry[KEY_SETTINGS].get(KEY_API_KEY).is_none(),
            "无凭证不写 apiKey"
        );
        assert!(
            entry[KEY_MODELS]["m1"].get(KEY_NAME).is_none(),
            "空显示名省略 name"
        );
        assert!(entry[KEY_MODELS]["m2"].get(KEY_NAME).is_none());
    }

    #[test]
    fn empty_provider_list_clears_namespace_only() {
        let input = r#"{
  // comment
  "providers": {
    "maestro-old": { "package": "@ai-sdk/openai-compatible" },
    "my-own": { "package": "@ai-sdk/openai-compatible" }
  }
}"#;
        let out = edit_config(input, &[]).unwrap();
        let doc = parse_jsonc(&out);
        assert_eq!(doc[KEY_PROVIDERS].as_object().unwrap().len(), 1);
        assert!(doc[KEY_PROVIDERS].get("maestro-old").is_none());
        assert!(out.contains("// comment"));
    }

    #[test]
    fn appends_providers_key_when_missing() {
        let out =
            edit_config("{\n  \"model\": \"some/model\"\n}\n", &[openai("foo", &[])]).unwrap();
        let doc = parse_jsonc(&out);
        assert_eq!(doc["model"], "some/model");
        assert_eq!(
            doc[KEY_PROVIDERS]["maestro-foo"][KEY_PACKAGE].as_str(),
            Some(PACKAGE_OPENAI)
        );
        assert!(
            doc[KEY_PROVIDERS]["maestro-foo"][KEY_MODELS]
                .as_object()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn rejects_broken_json() {
        let input = "{ \"providers\": { }\n"; // 缺右括号
        let err = edit_config(input, &[]).unwrap_err();
        assert!(err.contains("解析 opencode.json 失败"), "实际错误：{err}");
    }

    #[test]
    fn treats_empty_existing_file_as_new_document() {
        let out = edit_config("", &[openai("foo", &[])]).unwrap();
        let doc = parse_jsonc(&out);
        assert_eq!(doc[SCHEMA_KEY], SCHEMA_URL);
        assert_eq!(
            doc[KEY_PROVIDERS]["maestro-foo"][KEY_PACKAGE].as_str(),
            Some(PACKAGE_OPENAI)
        );
    }

    #[test]
    fn rejects_non_object_root() {
        let err = edit_config("[]", &[]).unwrap_err();
        assert!(err.contains("根节点不是对象"), "实际错误：{err}");
    }

    #[test]
    fn rejects_non_object_providers() {
        let err = edit_config("{ \"providers\": 42 }", &[]).unwrap_err();
        assert!(
            err.contains("providers 已存在但不是对象"),
            "实际错误：{err}"
        );
    }

    #[test]
    fn accepts_comments_and_trailing_commas_only() {
        // OpenCode 官方支持的 JSONC 扩展必须放行。
        let input = "{ // comment\n  \"model\": \"a\",\n  \"providers\": {},\n}";
        edit_config(input, &[]).unwrap();
    }

    #[test]
    fn rejects_syntax_beyond_jsonc() {
        // 缺逗号：jsonc-parser 默认允许，但 OpenCode 读不回，必须拒绝。
        let err = edit_config("{ \"model\": \"a\" \"providers\": {} }", &[]).unwrap_err();
        assert!(err.contains("解析 opencode.json 失败"), "实际错误：{err}");
        // 单引号字符串同理。
        let err = edit_config("{ 'model': 'a' }", &[]).unwrap_err();
        assert!(err.contains("解析 opencode.json 失败"), "实际错误：{err}");
    }

    #[test]
    fn selected_protocol_picks_its_endpoint() {
        // 双端点且选择指向 anthropic：用它，而非固定首选 openai（ADR 0016）。
        let p = dual_endpoint(
            "foo",
            "https://openai.example.com/v1",
            "https://anthropic.example.com",
            Some(Protocol::AnthropicMessages),
        );
        let out = edit_config("{}", &[p]).unwrap();
        let doc = parse_jsonc(&out);
        assert_eq!(
            doc[KEY_PROVIDERS]["maestro-foo"][KEY_PACKAGE].as_str(),
            Some(PACKAGE_ANTHROPIC)
        );
        assert_eq!(
            doc[KEY_PROVIDERS]["maestro-foo"][KEY_SETTINGS][KEY_BASE_URL],
            "https://anthropic.example.com/v1"
        );
    }

    #[test]
    fn falls_back_to_openai_when_selection_missing() {
        // 双端点且无选择（旧配置）：取固定顺序里的首选 openai-completions。
        let p = dual_endpoint(
            "foo",
            "https://openai.example.com/v1",
            "https://anthropic.example.com",
            None,
        );
        let out = edit_config("{}", &[p]).unwrap();
        let doc = parse_jsonc(&out);
        assert_eq!(
            doc[KEY_PROVIDERS]["maestro-foo"][KEY_PACKAGE].as_str(),
            Some(PACKAGE_OPENAI)
        );
        assert_eq!(
            doc[KEY_PROVIDERS]["maestro-foo"][KEY_SETTINGS][KEY_BASE_URL],
            "https://openai.example.com/v1"
        );
    }

    #[test]
    fn selection_pointing_to_absent_endpoint_falls_back_to_unique_endpoint() {
        // 只配了 anthropic，选择却指向 openai：选择失效，用唯一端点。
        let mut p = provider(
            "foo",
            Protocol::AnthropicMessages,
            "https://anthropic.example.com",
            None,
            &[("claude-5", None)],
        );
        p.selected_protocol = Some(Protocol::OpenaiCompletions);
        let out = edit_config("{}", &[p]).unwrap();
        let doc = parse_jsonc(&out);
        assert_eq!(
            doc[KEY_PROVIDERS]["maestro-foo"][KEY_PACKAGE].as_str(),
            Some(PACKAGE_ANTHROPIC)
        );
        assert_eq!(
            doc[KEY_PROVIDERS]["maestro-foo"][KEY_SETTINGS][KEY_BASE_URL],
            "https://anthropic.example.com/v1"
        );
    }

    /// 构造带 Provider 级自定义 header、指定 Model 列表的 Provider。
    fn rich_provider(headers: &[(&str, &str)], models: Vec<Model>) -> Provider {
        Provider {
            slug: "foo".to_owned(),
            endpoints: vec![Endpoint {
                protocol: Protocol::OpenaiCompletions,
                base_url: "https://gateway.example.com/v1".to_owned(),
            }],
            selected_protocol: None,
            api_key: None,
            custom_header: headers
                .iter()
                .map(|(name, value)| CustomHeader {
                    name: (*name).to_owned(),
                    value: (*value).to_owned(),
                })
                .collect(),
            models,
        }
    }

    /// 构造携带 token 上限与能力的 Model。
    fn rich_model(id: &str, limit: ModelLimit, capabilities: Vec<ModelCapability>) -> Model {
        Model {
            id: id.to_owned(),
            display_name: None,
            limit,
            capabilities,
        }
    }

    const NO_LIMIT: ModelLimit = ModelLimit {
        context_window: None,
        max_input: None,
        max_output: None,
    };

    #[test]
    fn projects_custom_headers_at_provider_level() {
        let p = rich_provider(&[("X-A", "1"), ("X-B", "2")], Vec::new());
        let out = edit_config("{}", &[p]).unwrap();
        let doc = parse_jsonc(&out);
        let entry = &doc[KEY_PROVIDERS]["maestro-foo"];
        assert_eq!(entry[KEY_HEADERS]["X-A"], "1");
        assert_eq!(entry[KEY_HEADERS]["X-B"], "2");
        assert!(
            entry[KEY_SETTINGS].get(KEY_HEADERS).is_none(),
            "headers 不再嵌在 settings 内"
        );
    }

    #[test]
    fn omits_headers_when_absent() {
        let out = edit_config("{}", &[rich_provider(&[], Vec::new())]).unwrap();
        let doc = parse_jsonc(&out);
        assert!(
            doc[KEY_PROVIDERS]["maestro-foo"].get(KEY_HEADERS).is_none(),
            "无自定义 header 时不应写 headers"
        );
    }

    #[test]
    fn projects_model_limits() {
        let m = rich_model(
            "gpt-5",
            ModelLimit {
                context_window: Some(200_000),
                max_input: Some(100_000),
                max_output: Some(8_192),
            },
            Vec::new(),
        );
        let out = edit_config("{}", &[rich_provider(&[], vec![m])]).unwrap();
        let doc = parse_jsonc(&out);
        let limit = &doc[KEY_PROVIDERS]["maestro-foo"][KEY_MODELS]["gpt-5"][KEY_LIMIT];
        assert_eq!(limit[KEY_CONTEXT], 200_000);
        assert_eq!(limit[KEY_INPUT], 100_000);
        assert_eq!(limit[KEY_OUTPUT], 8_192);
    }

    #[test]
    fn projects_limit_without_optional_input() {
        let m = rich_model(
            "m",
            ModelLimit {
                context_window: Some(1_000),
                max_input: None,
                max_output: Some(500),
            },
            Vec::new(),
        );
        let out = edit_config("{}", &[rich_provider(&[], vec![m])]).unwrap();
        let doc = parse_jsonc(&out);
        let limit = &doc[KEY_PROVIDERS]["maestro-foo"][KEY_MODELS]["m"][KEY_LIMIT];
        assert_eq!(limit[KEY_CONTEXT], 1_000);
        assert_eq!(limit[KEY_OUTPUT], 500);
        assert!(limit.get(KEY_INPUT).is_none(), "max_input 缺省时不写 input");
    }

    #[test]
    fn projects_partial_limits() {
        // OpenCode 2 的 limit 三个字段都可选，故各自写出，不再要求 context/output 同时存在。
        let models = vec![
            rich_model(
                "only_context",
                ModelLimit {
                    context_window: Some(1_000),
                    ..NO_LIMIT
                },
                Vec::new(),
            ),
            rich_model(
                "only_output",
                ModelLimit {
                    max_output: Some(500),
                    ..NO_LIMIT
                },
                Vec::new(),
            ),
            rich_model(
                "only_input",
                ModelLimit {
                    max_input: Some(20),
                    ..NO_LIMIT
                },
                Vec::new(),
            ),
            rich_model(
                "zero",
                ModelLimit {
                    context_window: Some(0),
                    max_output: Some(0),
                    ..NO_LIMIT
                },
                Vec::new(),
            ),
        ];
        let out = edit_config("{}", &[rich_provider(&[], models)]).unwrap();
        let doc = parse_jsonc(&out);
        let models = &doc[KEY_PROVIDERS]["maestro-foo"][KEY_MODELS];
        assert_eq!(models["only_context"][KEY_LIMIT][KEY_CONTEXT], 1_000);
        assert!(models["only_context"][KEY_LIMIT].get(KEY_OUTPUT).is_none());
        assert_eq!(models["only_output"][KEY_LIMIT][KEY_OUTPUT], 500);
        assert!(models["only_output"][KEY_LIMIT].get(KEY_CONTEXT).is_none());
        assert_eq!(models["only_input"][KEY_LIMIT][KEY_INPUT], 20);
        assert!(models["zero"].get(KEY_LIMIT).is_none(), "非正值不写 limit");
    }

    #[test]
    fn projects_model_capabilities() {
        let m = rich_model(
            "gpt-5",
            NO_LIMIT,
            vec![
                ModelCapability::ToolUse,
                ModelCapability::ImageIn,
                ModelCapability::Thinking,
            ],
        );
        let out = edit_config("{}", &[rich_provider(&[], vec![m])]).unwrap();
        let doc = parse_jsonc(&out);
        let entry = &doc[KEY_PROVIDERS]["maestro-foo"][KEY_MODELS]["gpt-5"];
        assert_eq!(entry[KEY_CAPABILITIES][KEY_TOOLS], true);
        assert_eq!(
            entry[KEY_CAPABILITIES][KEY_INPUT],
            serde_json::json!(["text", "image"])
        );
        // v2 已移除模型级 reasoning / attachment / modalities / tool_call 字段。
        for key in ["tool_call", "attachment", "modalities", "reasoning"] {
            assert!(entry.get(key).is_none(), "v2 不应写 {key}");
        }
    }

    #[test]
    fn omits_capability_fields_when_unset() {
        let m = rich_model("m", NO_LIMIT, Vec::new());
        let out = edit_config("{}", &[rich_provider(&[], vec![m])]).unwrap();
        let doc = parse_jsonc(&out);
        let entry = &doc[KEY_PROVIDERS]["maestro-foo"][KEY_MODELS]["m"];
        assert!(
            entry.get(KEY_CAPABILITIES).is_none(),
            "未设置能力时不写 capabilities"
        );
    }
}
