//! CMI AI Hub 默认模型提供方注入（AIHub 定制版）。
//!
//! dsh 核心的默认模型提供方由 `@deepseek-ai/dsh-base` 的 bundle patch 决定
//! （`agent-default-model` 行，出厂默认 `deepseek-official` / `deepseek-v4-flash`）。
//! 本模块在每次启动 dsh 前幂等地写入两层用户配置，把默认选择指向 CMI AI Hub
//! （patch 栈按「bundle → profile → home → overlay」顺序应用，最后写入者胜出）：
//!
//! 1. `$DSH_HOME/settings.yaml` 的 `llm-pi-ai.providers.cmi-aihub`：
//!    定义 provider 路由（baseURL、wire 协议、模型目录、API Key 环境变量引用）。
//!    dsh 的 `dsh-llm-pi-ai` 插件从该 settings 段读取 provider profiles，
//!    路由注册后模型选择器即可看到 CMI AI Hub 及其模型。
//! 2. `$DSH_HOME/cordis.patch.yml`（home 级用户层，优先级高于 profile 层）：
//!    覆盖 `agent-default-model` 行，把「新建 Agent 的默认选择」指向
//!    `cmi-aihub` / 默认模型。
//!
//! 幂等：仅当对应配置不存在时写入，绝不覆盖用户已有配置（用户后续在
//! Models 页面或 settings.yaml 中修改过的内容保持原样）。

use std::fs;
use std::path::{Path, PathBuf};

use tauri::AppHandle;

/// CMI AI Hub provider 路由 id（`llm-pi-ai.providers` 的键）。
const PROVIDER_ID: &str = "cmi-aihub";
/// CMI AI Hub 显示名（模型选择器 / 设置页展示）。
const PROVIDER_DISPLAY_NAME: &str = "CMI AI Hub";
/// CMI AI Hub API 地址（OpenAI 兼容）。
const PROVIDER_BASE_URL: &str = "https://mcloud-aihub.cmi.chinamobile.com/v1/";
/// wire 协议：OpenAI Chat Completions。
const PROVIDER_API: &str = "openai-completions";
/// API Key 环境变量名：用户需在系统环境变量中设置（如 `CMI_AIHUB_API_KEY=sk-...`）。
const API_KEY_ENV: &str = "CMI_AIHUB_API_KEY";
/// 默认模型 id（CMI AI Hub 实际开放的模型）。
const DEFAULT_MODEL_ID: &str = "azure/gpt-5-nano";
/// 默认模型显示名。
const DEFAULT_MODEL_NAME: &str = "GPT-5 Nano (Azure)";
/// 默认上下文窗口（token）。
const DEFAULT_CONTEXT_WINDOW: u64 = 131_072;
/// 默认最大输出 token。
const DEFAULT_MAX_TOKENS: u64 = 8_192;

/// settings.yaml 注入判定标记：出现该字符串即视为已注入。
const SETTINGS_MARKER: &str = "cmi-aihub";

/// 幂等地确保 CMI AI Hub 默认提供方配置已注入 `$DSH_HOME`。
///
/// 在 dsh 服务启动前调用；任何一步失败只记录日志，不阻断启动
/// （用户仍可在 Models 页面手动配置）。
pub fn ensure_default_provider(app_handle: &AppHandle) {
    let dsh_home = crate::config::get_dsh_data_path(app_handle);
    if let Err(e) = ensure_settings(&dsh_home) {
        log::warn!("cmi-aihub: settings.yaml 注入失败: {e}");
    }
    if let Err(e) = ensure_home_patch(&dsh_home) {
        log::warn!("cmi-aihub: cordis.patch.yml 注入失败: {e}");
    }
}

/// 写入 `$DSH_HOME/settings.yaml` 的 `llm-pi-ai.providers.cmi-aihub` 段。
fn ensure_settings(dsh_home: &Path) -> Result<(), String> {
    let path = dsh_home.join("settings.yaml");
    if path.exists() {
        let content = fs::read_to_string(&path).map_err(|e| format!("read failed: {e}"))?;
        if content.contains(SETTINGS_MARKER) {
            return Ok(()); // 已注入（或用户已配置），跳过
        }
    }

    // 解析现有文档（不存在则视为空 map），合并 cmi-aihub 段后写回。
    let mut root: serde_yaml::Value = if path.exists() {
        let content = fs::read_to_string(&path).map_err(|e| format!("read failed: {e}"))?;
        if content.trim().is_empty() {
            serde_yaml::Value::Mapping(Default::default())
        } else {
            serde_yaml::from_str(&content).map_err(|e| format!("parse failed: {e}"))?
        }
    } else {
        serde_yaml::Value::Mapping(Default::default())
    };

    let provider = serde_yaml::from_str::<serde_yaml::Value>(&format!(
        "apiKeyEnv: {API_KEY_ENV}\n\
         displayName: {PROVIDER_DISPLAY_NAME}\n\
         api: {PROVIDER_API}\n\
         baseURL: {PROVIDER_BASE_URL}\n\
         models:\n\
         \x20 - id: {DEFAULT_MODEL_ID}\n\
         \x20   name: {DEFAULT_MODEL_NAME}\n\
         \x20   contextWindow: {DEFAULT_CONTEXT_WINDOW}\n\
         \x20   maxTokens: {DEFAULT_MAX_TOKENS}\n\
         \x20   input: [text]\n"
    ))
    .map_err(|e| format!("provider yaml failed: {e}"))?;

    // root["llm-pi-ai"]["providers"][PROVIDER_ID] = provider
    let root_map = root
        .as_mapping_mut()
        .ok_or_else(|| "settings.yaml root must be a map".to_string())?;
    let llm_pi_ai = root_map
        .entry(serde_yaml::Value::String("llm-pi-ai".into()))
        .or_insert_with(|| serde_yaml::Value::Mapping(Default::default()));
    let llm_pi_ai_map = llm_pi_ai
        .as_mapping_mut()
        .ok_or_else(|| "llm-pi-ai must be a map".to_string())?;
    let providers = llm_pi_ai_map
        .entry(serde_yaml::Value::String("providers".into()))
        .or_insert_with(|| serde_yaml::Value::Mapping(Default::default()));
    let providers_map = providers
        .as_mapping_mut()
        .ok_or_else(|| "providers must be a map".to_string())?;
    providers_map.insert(serde_yaml::Value::String(PROVIDER_ID.into()), provider);

    let out = serde_yaml::to_string(&root).map_err(|e| format!("render failed: {e}"))?;
    write_file(&path, &out)?;
    log::info!("cmi-aihub: settings.yaml 已注入默认提供方 {PROVIDER_ID}");
    Ok(())
}

/// 写入 `$DSH_HOME/cordis.patch.yml`（home 级）的 `agent-default-model` 覆盖行。
///
/// 必须使用 id-targeted patch（`- id: agent-default-model` + `config`），不能使用
/// `- insert:`：bundle 层（dsh-base / dsh-cmi 的 cordis.patch.yml）已经 insert 了
/// `agent-default-model` 行，home 层再用 insert 会在 `mountRootInclude` 阶段触发
/// `duplicate loader entry id: agent-default-model`（0.10.7 实测，dsh 启动直接失败）。
/// id-targeted patch 按 id 覆盖 bundle 行，patch 栈「bundle → profile → home →
/// overlay」最后写入者胜出，不会产生重复 entry。
///
/// 迁移：0.10.7 曾写入 insert 块（特征：insert 数组含 id: agent-default-model 且
/// config.provider == cmi-aihub），本函数检测到即移除并改写为 id-targeted patch；
/// 用户自己写的 agent-default-model 条目（provider 非 cmi-aihub 的 insert，或
/// id-targeted patch）一律保留，绝不覆盖用户配置。
fn ensure_home_patch(dsh_home: &Path) -> Result<(), String> {
    let path = dsh_home.join("cordis.patch.yml");
    let existing = fs::read_to_string(&path).unwrap_or_default();

    // 顶层数组：空/纯注释视为空数组。
    let mut list: serde_yaml::Value = if existing.trim().is_empty() {
        serde_yaml::Value::Sequence(Vec::new())
    } else {
        let doc: serde_yaml::Value =
            serde_yaml::from_str(&existing).map_err(|e| format!("parse failed: {e}"))?;
        match doc {
            serde_yaml::Value::Sequence(_) => doc,
            serde_yaml::Value::Null => serde_yaml::Value::Sequence(Vec::new()),
            _ => return Err("cordis.patch.yml must be a top-level array".to_string()),
        }
    };

    let seq = list
        .as_sequence_mut()
        .ok_or_else(|| "cordis.patch.yml must be a sequence".to_string())?;

    // 1) 移除 0.10.7 写入的 insert 块；2) 用户自己的 agent-default-model 条目保留；
    // 3) 其余条目原样保留。
    let mut has_user_entry = false;
    let mut has_our_insert = false;
    let retained: Vec<serde_yaml::Value> = seq
        .iter()
        .filter(|el| {
            if block_is_our_insert(el) {
                has_our_insert = true;
                return false; // 移除我们 0.10.7 写入的 insert 块
            }
            if block_targets_agent_default_model(el) {
                has_user_entry = true;
            }
            true
        })
        .cloned()
        .collect();
    *seq = retained;

    // 用户已有自己的 agent-default-model 配置：保留原样；仅当移除了我们的
    // insert 块时才需要写回（其余情况无变化，不写盘、保留用户注释）。
    if has_user_entry {
        if has_our_insert {
            let out = serde_yaml::to_string(&list).map_err(|e| format!("render failed: {e}"))?;
            write_file(&path, &out)?;
            log::info!("cmi-aihub: 已移除 0.10.7 遗留的 agent-default-model insert 块");
        }
        return Ok(());
    }

    // 追加 id-targeted patch（覆盖 bundle 层的 insert，不产生重复 entry）。
    let patch = serde_yaml::from_str::<serde_yaml::Value>(&format!(
        "- id: agent-default-model\n\
         \x20 config:\n\
         \x20   provider: {PROVIDER_ID}\n\
         \x20   model: {DEFAULT_MODEL_ID}\n"
    ))
    .map_err(|e| format!("patch yaml failed: {e}"))?;
    // `- id: ...` 解析为单元素序列，取出其中的映射元素作为顶层数组项。
    let patch = match patch {
        serde_yaml::Value::Sequence(mut s) if s.len() == 1 => s.remove(0),
        other => other,
    };
    seq.push(patch);

    let out = serde_yaml::to_string(&list).map_err(|e| format!("render failed: {e}"))?;
    write_file(&path, &out)?;
    log::info!("cmi-aihub: cordis.patch.yml 已注入默认模型提供方 {PROVIDER_ID} (id-targeted patch)");
    Ok(())
}

/// 顶层数组元素是否为 0.10.7 写入的 `- insert:` 块（agent-default-model + cmi-aihub）。
fn block_is_our_insert(el: &serde_yaml::Value) -> bool {
    let Some(inserts) = el.get("insert").and_then(|v| v.as_sequence()) else {
        return false;
    };
    inserts.iter().any(|e| {
        e.get("id").and_then(|v| v.as_str()) == Some("agent-default-model")
            && e.get("config")
                .and_then(|c| c.get("provider"))
                .and_then(|v| v.as_str())
                == Some(PROVIDER_ID)
    })
}

/// 顶层数组元素是否为针对 `agent-default-model` 的条目（insert 或 id-targeted patch）。
fn block_targets_agent_default_model(el: &serde_yaml::Value) -> bool {
    if el.get("id").and_then(|v| v.as_str()) == Some("agent-default-model") {
        return true; // id-targeted patch
    }
    el.get("insert")
        .and_then(|v| v.as_sequence())
        .map(|inserts| {
            inserts
                .iter()
                .any(|e| e.get("id").and_then(|v| v.as_str()) == Some("agent-default-model"))
        })
        .unwrap_or(false)
}

/// 写入文件并创建父目录。
fn write_file(path: &Path, content: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create parent dir failed: {e}"))?;
    }
    fs::write(path, content).map_err(|e| format!("write {} failed: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_yaml_merges_into_existing_document() {
        let dir = std::env::temp_dir().join(format!("cmi-aihub-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // 预置用户已有配置（如 llm-deepseek 段）
        fs::write(
            dir.join("settings.yaml"),
            "llm-deepseek:\n  apiKeyEnv: DEEPSEEK_API_KEY\n",
        )
        .unwrap();

        ensure_settings(&dir).unwrap();

        let content = fs::read_to_string(dir.join("settings.yaml")).unwrap();
        assert!(content.contains("cmi-aihub"));
        assert!(content.contains("llm-deepseek")); // 用户配置保留
        assert!(content.contains("mcloud-aihub.cmi.chinamobile.com"));
        assert!(content.contains("openai-completions"));

        // 幂等：再次调用不重复追加
        ensure_settings(&dir).unwrap();
        let content2 = fs::read_to_string(dir.join("settings.yaml")).unwrap();
        assert_eq!(content.matches("cmi-aihub").count(), content2.matches("cmi-aihub").count());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn home_patch_appends_id_targeted_patch() {
        let dir = std::env::temp_dir().join(format!("cmi-aihub-patch-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        ensure_home_patch(&dir).unwrap();
        let content = fs::read_to_string(dir.join("cordis.patch.yml")).unwrap();
        assert!(content.contains("id: agent-default-model"));
        assert!(content.contains("provider: cmi-aihub"));
        assert!(content.contains("model: azure/gpt-5-nano"));
        // 必须用 id-targeted patch，不能是 insert（bundle 层已 insert，再 insert 会 duplicate）
        assert!(!content.contains("insert:"), "home patch must not use insert, got:\n{content}");

        // 幂等：再次调用不重复追加
        ensure_home_patch(&dir).unwrap();
        let content2 = fs::read_to_string(dir.join("cordis.patch.yml")).unwrap();
        assert_eq!(content.matches("agent-default-model").count(), content2.matches("agent-default-model").count());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn home_patch_migrates_legacy_insert_block() {
        let dir = std::env::temp_dir().join(format!("cmi-aihub-migrate-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // 0.10.7 写入的 insert 块（duplicate loader entry 根因）
        fs::write(
            dir.join("cordis.patch.yml"),
            "- insert:\n    - id: agent-default-model\n      name: '@deepseek-ai/dsh-agent-default-model'\n      config:\n        provider: cmi-aihub\n        model: azure/gpt-5-nano\n",
        )
        .unwrap();

        ensure_home_patch(&dir).unwrap();
        let content = fs::read_to_string(dir.join("cordis.patch.yml")).unwrap();
        assert!(!content.contains("insert:"), "legacy insert block must be removed, got:\n{content}");
        assert!(content.contains("id: agent-default-model"));
        assert!(content.contains("provider: cmi-aihub"));

        // 幂等：再次调用不再变化
        ensure_home_patch(&dir).unwrap();
        let content2 = fs::read_to_string(dir.join("cordis.patch.yml")).unwrap();
        assert_eq!(content, content2);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn home_patch_preserves_user_entry() {
        let dir = std::env::temp_dir().join(format!("cmi-aihub-user-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // 用户自己的 id-targeted patch（provider 非 cmi-aihub）
        fs::write(
            dir.join("cordis.patch.yml"),
            "- id: agent-default-model\n  config:\n    provider: deepseek-official\n    model: deepseek-v4-flash\n",
        )
        .unwrap();

        ensure_home_patch(&dir).unwrap();
        let content = fs::read_to_string(dir.join("cordis.patch.yml")).unwrap();
        assert!(content.contains("provider: deepseek-official"));
        assert!(!content.contains("provider: cmi-aihub"), "user config must not be overwritten, got:\n{content}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn home_patch_preserves_user_insert() {
        let dir = std::env::temp_dir().join(format!("cmi-aihub-userins-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // 用户自己的 insert 块（provider 非 cmi-aihub）
        fs::write(
            dir.join("cordis.patch.yml"),
            "- insert:\n    - id: agent-default-model\n      name: '@deepseek-ai/dsh-agent-default-model'\n      config:\n        provider: deepseek-official\n        model: deepseek-v4-flash\n",
        )
        .unwrap();

        ensure_home_patch(&dir).unwrap();
        let content = fs::read_to_string(dir.join("cordis.patch.yml")).unwrap();
        assert!(content.contains("provider: deepseek-official"));
        assert!(!content.contains("provider: cmi-aihub"), "user insert must not be overwritten, got:\n{content}");

        let _ = fs::remove_dir_all(&dir);
    }
}
