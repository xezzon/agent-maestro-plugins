//! Kimi Code 插件：把 Maestro 录入的 Provider 投影为 `~/.kimi-code/config.toml` 中的
//! `[providers."maestro:<slug>"]` 与 `[models."maestro:<slug>/<model-id>"]` 条目。
//!
//! 与 pi 插件的整文件重写不同，`config.toml` 是 Kimi Code 与用户共享的配置文件
//! （`default_model`、`thinking`、`permission` 等都住在里面），因此本插件只重建
//! `maestro:` 命名空间下的条目：先删除该命名空间的旧条目再写入当前列表，
//! 其余内容经 `toml_edit` 无损保留（注释、格式、非插件条目原样不动）。
//! 命名空间约定参照 Kimi Code 自带的 `managed:kimi-code` provider 命名。
//!
//! `max_context_size` 是 Kimi Code 的必填字段而 Maestro 的 Model 不携带该信息，
//! 插件写入固定默认值 [`DEFAULT_MAX_CONTEXT_SIZE`]；如与实际不符，用户可在
//! `[models."<alias>".overrides]` 中覆盖。
//!
//! 依赖 [`maestro-plugin-sdk`]（`maestro:plugin` 合同的类型化绑定），实现其
//! `Guest` trait。宿主把 manifest 声明的 config_dir（即 `$HOME/.kimi-code`）预开放
//! 为 "/"，插件以相对路径读写。注意 Kimi Code 支持 `KIMI_CODE_HOME` 重定向数据
//! 目录，而插件 config_dir 由 manifest 静态声明，重定向场景不在支持范围内。

use std::fs;
use std::path::Path;

use maestro_plugin_sdk::{Guest, Model, Protocol, Provider, export};
use toml_edit::{DocumentMut, Item, Table, value};

/// 插件写入的唯一文件（相对 config_dir）。
const CONFIG_FILE: &str = "config.toml";
/// 插件条目的命名空间前缀：provider 名与模型别名都以此为前缀，
/// 投影时据此识别并清理旧条目，避免误伤用户手工配置的条目。
const NAMESPACE: &str = "maestro:";
/// `max_context_size` 的固定默认值：Maestro 的 Model 不携带上下文长度信息，
/// 而 Kimi Code 要求该字段必填。取保守值（128k），声明偏小只会让 Kimi Code
/// 提前压缩上下文，不会引发服务端报错；用户可用 model overrides 覆盖。
const DEFAULT_MAX_CONTEXT_SIZE: i64 = 131072;

const KEY_TYPE: &str = "type";
const KEY_API_KEY: &str = "api_key";
const KEY_BASE_URL: &str = "base_url";
const KEY_PROVIDER: &str = "provider";
const KEY_MODEL: &str = "model";
const KEY_MAX_CONTEXT_SIZE: &str = "max_context_size";
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

    {
        let providers_table = ensure_table(&mut doc, TABLE_PROVIDERS)?;
        remove_namespace_entries(providers_table);
        for provider in providers {
            let provider_name = format!("{NAMESPACE}{}", provider.slug);
            providers_table.insert(&provider_name, provider_table(provider));
        }
    }
    {
        let models_table = ensure_table(&mut doc, TABLE_MODELS)?;
        remove_namespace_entries(models_table);
        for provider in providers {
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

fn provider_table(provider: &Provider) -> Item {
    let mut entry = Table::new();
    entry.insert(KEY_TYPE, value(provider_type(&provider.protocol)));
    entry.insert(KEY_BASE_URL, value(provider.base_url.as_str()));
    // 无凭证时宿主传 None，本地网关模型在 Kimi Code 中仍可见；
    // 用户可自行补 api_key 或 env 兜底。
    if let Some(api_key) = &provider.api_key {
        entry.insert(KEY_API_KEY, value(api_key.as_str()));
    }
    Item::Table(entry)
}

fn model_table(provider_name: &str, model: &Model) -> Item {
    let mut entry = Table::new();
    entry.insert(KEY_PROVIDER, value(provider_name));
    entry.insert(KEY_MODEL, value(model.id.as_str()));
    entry.insert(KEY_MAX_CONTEXT_SIZE, value(DEFAULT_MAX_CONTEXT_SIZE));
    if let Some(name) = &model.display_name
        && !name.is_empty()
    {
        entry.insert(KEY_DISPLAY_NAME, value(name.as_str()));
    }
    Item::Table(entry)
}

export!(KimiCodePlugin);
