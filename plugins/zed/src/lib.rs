//! Zed 插件：把 Maestro 录入的 Provider 投影为 `$HOME/.config/zed/settings.json` 中
//! `language_models.openai_compatible."maestro-<slug>"` 与
//! `language_models.anthropic_compatible."maestro-<slug>"` 条目。
//!
//! 与 pi 插件的整文件重写不同，`settings.json` 是 Zed 与用户共享的配置文件
//! （Zed 自己写入注释头与各种设置，用户也会往里加主题、键位、自定义 provider），
//! 因此本插件只重建 `maestro-` 命名空间下的条目：先删除该命名空间的旧条目再写入
//! 当前列表，其余内容经 `jsonc-parser` 的 CST 无损保留（注释、缩进、非插件条目
//! 原样不动）。命名空间用 `-` 而不是 `:`：Zed 会用 provider id 推导取凭证用的
//! 环境变量名（`convert_case` 的 UpperSnake），冒号会落进变量名而无法在 shell 中使用。
//!
//! # 凭证无法投影
//!
//! Zed 的 `openai_compatible` / `anthropic_compatible` 条目没有 api_key 字段，
//! Maestro 中录入的 API Key 因此落不了盘：Zed 只从操作系统钥匙串（按 api_url 索引，
//! 经 Zed 自己的模型/provider 设置界面写入）或环境变量 `<PROVIDER_ID>_API_KEY`
//! （provider id 转 UPPER_SNAKE）取凭证。插件既不伪造 api_key 字段，也不把凭证塞进
//! `custom_headers`——Zed 在取到凭证之前就会以 `NoApiKey` 直接失败。README 记录了
//! 上面两种补救方式。
//!
//! `max_tokens` 是 Zed 的必填字段（语义为上下文窗口大小）而 Maestro 的 Model 不携带
//! 该信息，插件写入固定默认值 [`DEFAULT_MAX_TOKENS`]。
//!
//! 依赖 [`maestro_plugin_sdk`]（`maestro:plugin` 合同的类型化绑定），实现其
//! `Guest` trait。宿主把 manifest 声明的 config_dir（即 `$HOME/.config/zed`）预开放
//! 为 "/"，插件以相对路径读写。注意 Windows 上 Zed 的配置目录是 `%APPDATA%\Zed`，
//! `$XDG_CONFIG_HOME` 重定向同理不在支持范围内——config_dir 由 manifest 静态声明。

use std::fs;
use std::path::Path;

use jsonc_parser::ParseOptions;
use jsonc_parser::cst::{CstInputValue, CstObject, CstObjectProp, CstRootNode};
use maestro_plugin_sdk::{Guest, Model, Protocol, Provider, export};

/// 插件写入的唯一文件（相对 config_dir）。
const SETTINGS_FILE: &str = "settings.json";
/// 插件条目的命名空间前缀：provider id 以此为前缀，投影时据此识别并清理旧条目，
/// 避免误伤用户手工配置的 provider。
const NAMESPACE: &str = "maestro-";
/// `max_tokens` 的固定默认值：该字段在 Zed 中必填（上下文窗口大小），
/// 而 Maestro 的 Model 不携带该信息。取保守值（128k），声明偏小只会让 Zed 提前
/// 压缩上下文，不会引发服务端报错；用户可在 Zed 设置里自行改大。
const DEFAULT_MAX_TOKENS: i64 = 131072;

const SECTION_LANGUAGE_MODELS: &str = "language_models";
const SECTION_OPENAI: &str = "openai_compatible";
const SECTION_ANTHROPIC: &str = "anthropic_compatible";
const KEY_API_URL: &str = "api_url";
const KEY_AVAILABLE_MODELS: &str = "available_models";
const KEY_NAME: &str = "name";
const KEY_DISPLAY_NAME: &str = "display_name";
const KEY_MAX_TOKENS: &str = "max_tokens";

struct ZedPlugin;

impl Guest for ZedPlugin {
    fn write_providers(providers: Vec<Provider>) -> Result<Vec<String>, String> {
        write_settings(&providers)?;
        Ok(vec![SETTINGS_FILE.to_owned()])
    }
}

fn write_settings(providers: &[Provider]) -> Result<(), String> {
    let path = Path::new(SETTINGS_FILE);
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("读取 settings.json 失败\n原因：{e}")),
    };
    let root = CstRootNode::parse(&text, &ParseOptions::default())
        .map_err(|e| format!("解析 settings.json 失败，为避免破坏既有配置已放弃写入\n原因：{e}"))?;
    // 空文件没有根值，可初始化为对象；根值已存在但不是对象则放弃写入。
    // 注意最后必须写回 root（而非 doc）：文件头尾的注释等游离内容挂在根节点上。
    let doc = match root.value() {
        None => root.object_value_or_set(),
        Some(_) => root.object_value().ok_or_else(|| {
            "settings.json 的根节点不是对象，为避免破坏既有配置已放弃写入".to_owned()
        })?,
    };

    // 一个 Provider 只属于一个协议槽位，故按协议分两组各写一个 section。
    let mut openai = Vec::new();
    let mut anthropic = Vec::new();
    for provider in providers {
        let id = format!("{NAMESPACE}{}", provider.slug);
        let entry = (id, provider_value(provider));
        match provider.protocol {
            Protocol::OpenaiCompletions => openai.push(entry),
            Protocol::AnthropicMessages => anthropic.push(entry),
        }
    }

    let section = match doc.object_value(SECTION_LANGUAGE_MODELS) {
        Some(section) => Some(section),
        None if doc.get(SECTION_LANGUAGE_MODELS).is_some() => {
            return Err(format!(
                "settings.json 中 {SECTION_LANGUAGE_MODELS} 已存在但不是对象，为避免破坏既有配置已放弃写入"
            ));
        }
        // 本就没有 language_models 且本轮无条目可写：不创建空壳 section。
        None if openai.is_empty() && anthropic.is_empty() => None,
        None => doc
            .object_value_or_create(SECTION_LANGUAGE_MODELS)
            .expect("刚创建的 language_models 是对象")
            .into(),
    };

    if let Some(section) = section {
        let purged_openai = project_section(&section, SECTION_OPENAI, openai)?;
        let purged_anthropic = project_section(&section, SECTION_ANTHROPIC, anthropic)?;
        // 两个 section 都是插件清空后留下的空壳时，连同 language_models 一起移除。
        if (purged_openai || purged_anthropic) && section.properties().is_empty() {
            doc.get(SECTION_LANGUAGE_MODELS)
                .expect("language_models 存在")
                .remove();
        }
    }

    // 原子写入：先写临时文件成功后再 rename，I/O 错误不会留下半截的 settings.json。
    let tmp = Path::new("settings.json.tmp");
    fs::write(tmp, root.to_string())
        .map_err(|e| format!("写入 settings.json 临时文件失败\n原因：{e}"))?;
    fs::rename(tmp, path).map_err(|e| format!("替换 settings.json 失败\n原因：{e}"))?;

    Ok(())
}

/// 在 `parent[name]` 上重建 `maestro-` 命名空间：先删掉上一轮投影的条目，
/// 再写入本轮条目；section 因清空而变空时连同属性一起移除。
/// 返回本次是否清掉过旧条目。
fn project_section(
    parent: &CstObject,
    name: &str,
    entries: Vec<(String, CstInputValue)>,
) -> Result<bool, String> {
    let section = match parent.object_value(name) {
        Some(section) => section,
        None if parent.get(name).is_some() => {
            return Err(format!(
                "settings.json 中 {SECTION_LANGUAGE_MODELS}.{name} 已存在但不是对象，\
                 为避免破坏既有配置已放弃写入"
            ));
        }
        // 没有该 section 时不制造空壳，只有确实有条目才创建。
        None if entries.is_empty() => return Ok(false),
        None => parent
            .object_value_or_create(name)
            .expect("刚创建的 section 是对象"),
    };

    let stale: Vec<CstObjectProp> = section
        .properties()
        .into_iter()
        .filter(|prop| {
            prop.decoded_name()
                .is_some_and(|name| name.starts_with(NAMESPACE))
        })
        .collect();
    let purged = !stale.is_empty();
    for prop in stale {
        prop.remove();
    }
    for (id, entry) in entries {
        section.append(&id, entry);
    }

    if purged && section.properties().is_empty() {
        parent.get(name).expect("section 存在").remove();
    }
    Ok(purged)
}

/// Provider 条目：`{ api_url, available_models }`。
/// API Key 无处可写（见模块文档）；`capabilities` 在 Zed 侧有默认值，Maestro 也不携带
/// 该信息，故省略。
fn provider_value(provider: &Provider) -> CstInputValue {
    CstInputValue::Object(vec![
        (
            KEY_API_URL.to_owned(),
            CstInputValue::String(provider.base_url.clone()),
        ),
        (
            KEY_AVAILABLE_MODELS.to_owned(),
            CstInputValue::Array(provider.models.iter().map(model_value).collect()),
        ),
    ])
}

/// 模型条目：`{ name, display_name?, max_tokens }`。
fn model_value(model: &Model) -> CstInputValue {
    let mut props = vec![(KEY_NAME.to_owned(), CstInputValue::String(model.id.clone()))];
    if let Some(display_name) = &model.display_name
        && !display_name.is_empty()
    {
        props.push((
            KEY_DISPLAY_NAME.to_owned(),
            CstInputValue::String(display_name.clone()),
        ));
    }
    props.push((
        KEY_MAX_TOKENS.to_owned(),
        CstInputValue::Number(DEFAULT_MAX_TOKENS.to_string()),
    ));
    CstInputValue::Object(props)
}

export!(ZedPlugin);
