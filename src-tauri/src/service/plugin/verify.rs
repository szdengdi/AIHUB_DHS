//! 预装插件完整性自检与离线修复（verify）。
//!
//! 动机（issue #90）：首次引导「安装全部推荐插件」后，若安装只把插件写入了
//! profile 清单（`package.json` 的 `dependencies` + `dsh.profile.bundles`）而
//! `node_modules` 中的插件产物缺失/损坏（安装中断、磁盘异常、半失败状态等），
//! dsh 服务启动时 loader 会对每个缺失插件抛 `ERR_MODULE_NOT_FOUND`（日志特征
//! `Cannot find package 'X' imported from .../cordis-plugin-loader/lib/index.js`），
//! 整个插件树加载失败、服务无法启动；更糟的是 pnpm 在损坏的 node_modules 上
//! 同样无法卸载/升级，用户被卡死在「启动失败 + 卸载失败」的组合里。
//!
//! 本模块在服务启动前核对「清单引用的预装插件是否真实存在于 profile 的
//! `node_modules`」，缺失时以现有 manifest + lockfile 为准在 profile 目录执行
//! `pnpm install` 重建依赖图（不解析新版本、不写清单，纯修复节点_modules 与
//! 锁文件一致）；修复后仍缺失再给对应插件记录错误标记（前端插件面板显示异常，
//! 配合 [`super::install::remove`] 的离线卸载兜底即可移除问题插件）。
//!
//! 全程最佳努力：任何失败只记日志/错误标记，不阻断启动——最终启动失败场景仍
//! 由前端 recovery 对话框（`recovery.rs`）兜底定位与卸载。

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::AppHandle;

use crate::config;
use crate::service::cli;

use super::errors;
use super::install::{build_plugin_envs, harness_prefer_bundled_pnpm};
use super::installed::{installed_name, profile_dir, ProfilePackageJson};
use super::preset::{load_presets, PreinstallPluginInfo};

/// 判定清单引用的预装插件是否缺失产物（纯函数，便于单测）。
///
/// 命中条件：插件被 profile 清单引用（`dependencies` 键或 `dsh.profile.bundles`
/// 列表，按实际 npm 包名 `installed_name`），但其包目录在 `node_modules` 下
/// 缺少 `package.json`（目录不存在、被删、或只是残壳都不算就绪）。
fn missing_plugin_ids(
    presets: &[PreinstallPluginInfo],
    dependencies: &HashMap<String, String>,
    bundles: &[String],
    node_modules: &Path,
) -> Vec<String> {
    presets
        .iter()
        .filter(|preset| {
            let name = installed_name(preset);
            let referenced =
                dependencies.contains_key(name) || bundles.iter().any(|bundle| bundle == name);
            if !referenced {
                return false;
            }
            !node_modules.join(name).join("package.json").is_file()
        })
        .map(|preset| preset.id.clone())
        .collect()
}

/// 给指定的缺失插件记录错误标记（前端插件面板显示异常并可卸载兜底）。
fn record_missing(app_handle: &AppHandle, ids: &[String], detail: &str) {
    for id in ids {
        if let Err(e) = errors::record(app_handle, id, "install", detail) {
            log::warn!("failed to record plugin integrity error for {id}: {e}");
        }
    }
}

/// 启动前调用：核对并修复预装插件完整性。最佳努力：失败只返回 Err（调用方告警）。
pub(crate) async fn ensure_preset_plugins(app_handle: &AppHandle) -> Result<(), String> {
    let presets = load_presets(app_handle);
    if presets.is_empty() {
        return Ok(());
    }

    let profile = profile_dir(app_handle);
    // 读取清单（dependencies + bundles）；档案未初始化（无 package.json /
    // 不可解析）视为尚未安装任何插件，无需修复。
    let (dependencies, bundles) = match std::fs::read_to_string(profile.join("package.json")) {
        Ok(raw) => match serde_json::from_str::<ProfilePackageJson>(&raw) {
            Ok(manifest) => (
                manifest.dependencies,
                manifest
                    .dsh
                    .and_then(|d| d.profile)
                    .map(|p| p.bundles)
                    .unwrap_or_default(),
            ),
            Err(_) => return Ok(()),
        },
        Err(_) => return Ok(()),
    };

    let node_modules = profile.join("node_modules");
    let missing = missing_plugin_ids(&presets, &dependencies, &bundles, &node_modules);
    if missing.is_empty() {
        return Ok(());
    }
    log::warn!(
        "PRESET_PLUGIN_INTEGRITY: {} preset plugin(s) referenced by profile but missing from node_modules: {missing:?}",
        missing.len()
    );

    // 修复：在 profile 目录以现有 manifest + lockfile 为准执行 `pnpm install`，
    // 重建 node_modules 依赖图（不解析新版本）。修复失败不阻断启动，给缺失插件
    // 记录错误标记，让前端插件面板暴露问题、用户可走卸载兜底恢复。
    if let Err(e) = repair_with_pnpm_install(app_handle, &profile).await {
        log::warn!("PRESET_PLUGIN_REPAIR_FAILED: {e}");
        // 离线/无 pnpm 环境下无法重建依赖图：清理缺失插件的引用
        // （package.json 的 dependencies/bundles + cordis.patch.yml 挂载块），
        // 否则 dsh 的 loadProfile 会对 bundles 中无法解析的社区插件直接抛
        // `cannot resolve profile bundle` 而无法启动（0.10.7 实测）。
        prune_unavailable_plugins(&profile);
        record_missing(app_handle, &missing, &e);
        return Ok(());
    }

    // 复检：dependencies/bundles 未变（pnpm install 不写清单），只需重新探测产物。
    let still_missing = missing_plugin_ids(&presets, &dependencies, &bundles, &node_modules);
    if still_missing.is_empty() {
        log::info!("preset plugin integrity repaired via pnpm install: {missing:?}");
        return Ok(());
    }
    // pnpm install 后仍缺失（如离线环境无法下载）：同样清理引用，保证启动。
    prune_unavailable_plugins(&profile);
    let detail = format!(
        "PRESET_PLUGIN_STILL_MISSING: 修复后仍缺失以下插件产物: {still_missing:?}。可尝试卸载后重新安装。"
    );
    log::warn!("{detail}");
    record_missing(app_handle, &still_missing, &detail);
    Ok(())
}

/// 清理 profile 中「清单引用但产物缺失」的社区插件：从 package.json 的
/// dependencies/bundles 移除，并从 cordis.patch.yml 移除对应挂载块。
///
/// 只处理非 `@deepseek-ai/` 的包（官方 bundle 由 dsh 安装目录提供，profile
/// node_modules 缺失是正常布局）；社区插件（dsh-better-sidebar、dshmarket、
/// dsh-win-terminal-inspector 等）在离线/损坏环境下无法解析时，保留引用会让
/// dsh 的 loadProfile 直接抛 `cannot resolve profile bundle` 而无法启动。
/// 最佳努力：任何失败只记日志，不阻断启动。
fn prune_unavailable_plugins(profile: &Path) {
    match prune_unavailable_plugins_inner(profile) {
        Ok(pruned) if !pruned.is_empty() => {
            log::warn!("PRESET_PLUGIN_PRUNE: 已清理不可用插件引用: {pruned:?}");
        }
        Ok(_) => {}
        Err(e) => log::warn!("PRESET_PLUGIN_PRUNE_FAILED: {e}"),
    }
}

/// [`prune_unavailable_plugins`] 的实现；返回被清理的包名列表。
fn prune_unavailable_plugins_inner(profile: &Path) -> Result<Vec<String>, String> {
    let manifest_path = profile.join("package.json");
    let raw = match std::fs::read_to_string(&manifest_path) {
        Ok(raw) => raw,
        Err(_) => return Ok(Vec::new()), // 无清单则无需清理
    };
    let mut manifest: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| format!("PRUNE_MANIFEST_INVALID: {manifest_path:?}: {e}"))?;

    // 收集引用名（dependencies keys + bundles）。
    let mut referenced: Vec<String> = Vec::new();
    if let Some(deps) = manifest.get("dependencies").and_then(|v| v.as_object()) {
        referenced.extend(deps.keys().cloned());
    }
    if let Some(bundles) = manifest
        .get("dsh")
        .and_then(|v| v.get("profile"))
        .and_then(|v| v.get("bundles"))
        .and_then(|v| v.as_array())
    {
        referenced.extend(bundles.iter().filter_map(|v| v.as_str().map(str::to_owned)));
    }

    // 找出缺失的社区插件（非 @deepseek-ai/ 且 profile node_modules 无产物）。
    let node_modules = profile.join("node_modules");
    let mut unavailable: Vec<String> = Vec::new();
    for name in &referenced {
        if name.starts_with("@deepseek-ai/") {
            continue;
        }
        if !node_modules.join(name).join("package.json").is_file() {
            unavailable.push(name.clone());
        }
    }
    if unavailable.is_empty() {
        return Ok(Vec::new());
    }

    // 从 dependencies 移除。
    if let Some(deps) = manifest.get_mut("dependencies").and_then(|v| v.as_object_mut()) {
        for name in &unavailable {
            deps.remove(name);
        }
    }
    // 从 bundles 移除。
    if let Some(bundles) = manifest
        .get_mut("dsh")
        .and_then(|v| v.get_mut("profile"))
        .and_then(|v| v.get_mut("bundles"))
        .and_then(|v| v.as_array_mut())
    {
        bundles.retain(|v| {
            v.as_str()
                .map(|s| !unavailable.iter().any(|u| u == s))
                .unwrap_or(true)
        });
    }

    // 写回 package.json。
    let out = serde_json::to_string_pretty(&manifest)
        .map_err(|e| format!("PRUNE_MANIFEST_RENDER: {e}"))?;
    std::fs::write(&manifest_path, out + "\n")
        .map_err(|e| format!("PRUNE_MANIFEST_WRITE: {manifest_path:?}: {e}"))?;

    // 清理 cordis.patch.yml 中对应挂载块。
    prune_patch_blocks(profile, &unavailable)?;

    Ok(unavailable)
}

/// 从 profile 的 cordis.patch.yml 移除引用指定包名的 insert 块。
fn prune_patch_blocks(profile: &Path, names: &[String]) -> Result<(), String> {
    let patch_path = profile.join("cordis.patch.yml");
    let existing = match std::fs::read_to_string(&patch_path) {
        Ok(s) => s,
        Err(_) => return Ok(()), // 无 patch 文件则无需清理
    };
    let doc = match serde_yaml::from_str::<serde_yaml::Value>(&existing) {
        Ok(d) => d,
        Err(_) => return Ok(()), // 无法解析则不动原文件
    };
    let serde_yaml::Value::Sequence(seq) = doc else {
        return Ok(());
    };
    let original_len = seq.len();
    let retained: Vec<serde_yaml::Value> = seq
        .into_iter()
        .filter(|el| !block_references_any(el, names))
        .collect();
    if retained.len() == original_len {
        return Ok(()); // 无变化
    }
    let out = serde_yaml::to_string(&serde_yaml::Value::Sequence(retained))
        .map_err(|e| format!("PRUNE_PATCH_RENDER: {e}"))?;
    std::fs::write(&patch_path, out).map_err(|e| format!("PRUNE_PATCH_WRITE: {patch_path:?}: {e}"))
}

/// 顶层数组元素（insert 块）是否引用指定包名（按 name 字符串包含匹配）。
fn block_references_any(el: &serde_yaml::Value, names: &[String]) -> bool {
    let Some(inserts) = el.get("insert").and_then(|v| v.as_sequence()) else {
        return false;
    };
    inserts.iter().any(|e| {
        let Some(name) = e.get("name").and_then(|v| v.as_str()) else {
            return false;
        };
        names.iter().any(|n| name.contains(n.as_str()))
    })
}

/// 在 profile 目录执行 `pnpm install` 修复 node_modules 依赖图。
///
/// 选定直接执行的 pnpm（见 [`pnpm_direct`]），环境沿用桌面端插件子进程策略
/// （`build_plugin_envs`：隔离 $DSH_HOME、PATH 前置 shim 与 node 目录、pnpm
/// 选版开关），与安装/升级/卸载同一套行为，避免意外的 store/版本不兼容。
async fn repair_with_pnpm_install(app_handle: &AppHandle, profile: &Path) -> Result<(), String> {
    let (program, mut args) = pnpm_direct(app_handle).ok_or_else(|| {
        "PNPM_NOT_FOUND: 无捆绑 pnpm 且无用户 pnpm，无法修复插件依赖（如需可手动执行 pnpm install）"
            .to_string()
    })?;
    args.push(OsString::from("install"));

    let envs = build_plugin_envs(app_handle, harness_prefer_bundled_pnpm(app_handle));
    log::info!(
        "repairing profile node_modules via pnpm install in {}",
        profile.display()
    );
    let (exit_code, output) = spawn_and_wait(&program, &args, profile, &envs).await?;
    if exit_code != 0 {
        // 日志只保留输出尾部，避免刷屏（pnpm 失败信息集中在末尾）。
        let tail: String = output
            .chars()
            .rev()
            .take(REPAIR_OUTPUT_LIMIT)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        return Err(format!(
            "PNPM_INSTALL_FAILED: exit code {exit_code}: {tail}"
        ));
    }
    Ok(())
}

/// 修复输出日志保留的最大字符数（pnpm 失败信息集中在输出末尾）。
const REPAIR_OUTPUT_LIMIT: usize = 4000;

/// 选定修复用的 pnpm 直接执行程序：(program, 前置参数)。
///
/// 与 [`super::install::ensure_pnpm`] 同一套 store 主版本感知策略（pnpm 10 与 11
/// 的 store 布局互不兼容，主版本不一致会 `ERR_PNPM_UNEXPECTED_STORE` 失败），
/// 但修复路径**不触发下载**（启动阶段零额外下载）：
/// - 档案 store 主版本已知 → 优先选主版本一致的 pnpm（捆绑版 → 用户版）；
/// - store 未知（全新档案/未装过依赖）→ 捆绑版优先，否则用户版；
/// - 全不匹配时仍退回捆绑版尽力一试（失败由调用方记录错误标记）。
///
/// Windows 上 `.cmd`/`.bat` 无法被 CreateProcess 直接执行（需 cmd.exe 解析），
/// 用户 pnpm 只接受 `.exe`。
pub(crate) fn pnpm_direct(app_handle: &AppHandle) -> Option<(PathBuf, Vec<OsString>)> {
    use super::install::{bundled_pnpm_major, pnpm_major_version_at, profile_store_major};

    let bundled = config::get_pnpm_binary_path(app_handle);
    let bundled_ready = bundled.exists();
    let store = profile_store_major(app_handle);
    let bundled_matches =
        bundled_ready && store.is_none_or(|s| bundled_pnpm_major(app_handle) == Some(s));

    if bundled_matches {
        let node = config::get_node_binary_path(app_handle);
        return Some((node, vec![bundled.into_os_string()]));
    }

    // 用户 pnpm 主版本与 store 一致（或 store 未知）都可直接用。Windows
    // CreateProcess 不能直接执行 `.cmd`/`.bat`，因此专门查找原生 pnpm.exe。
    #[cfg(windows)]
    let user_pnpm = cli::find_user_pnpm_executable(app_handle);
    #[cfg(not(windows))]
    let user_pnpm = cli::find_user_pnpm(app_handle);
    if let Some(user) = user_pnpm {
        if store.is_none_or(|s| pnpm_major_version_at(&user) == Some(s)) {
            return Some((user, Vec::new()));
        }
    }

    // 都不匹配：退回捆绑版尽力一试（pnpm 会因 store 不兼容失败，由调用方记录）
    if bundled_ready {
        let node = config::get_node_binary_path(app_handle);
        return Some((node, vec![bundled.into_os_string()]));
    }
    None
}

/// 启动子进程 `program args...`（cwd=`cwd`）并等待退出，返回 (退出码, 合并输出)。
///
/// Windows 复用隐藏控制台方案（`workflow::win_spawn`）：GUI 进程直接以
/// CREATE_NO_WINDOW 启动会让 node 的子进程各自新建可见控制台窗口（黑窗闪烁）。
/// 原始进程句柄（`*mut c_void`）非 Send，整个「spawn + 读管道 + 等待」都在单个
/// `spawn_blocking` 内完成（与 `plugin::process` 的等待语义一致），句柄不外传。
async fn spawn_and_wait(
    program: &Path,
    args: &[OsString],
    cwd: &Path,
    envs: &HashMap<String, String>,
) -> Result<(i32, String), String> {
    let captured = Arc::new(Mutex::new(String::new()));

    #[cfg(windows)]
    {
        use crate::service::workflow;
        use std::time::Duration;

        let program = program.to_path_buf();
        let args = args.to_vec();
        let cwd = cwd.to_path_buf();
        let envs = envs.clone();
        let captured = captured.clone();
        let (exit_code, output) = tauri::async_runtime::spawn_blocking(move || {
            use windows_sys::Win32::Foundation::CloseHandle;
            use windows_sys::Win32::System::Threading::{
                GetExitCodeProcess, WaitForSingleObject, INFINITE,
            };

            let (stdout, stderr, handle) = workflow::win_spawn::spawn_with_hidden_console_tracked(
                &program,
                &args,
                Some(&cwd),
                &envs,
            )
            .map_err(|e| format!("PNPM_REPAIR_SPAWN: {e}"))?;
            drain_pipe(stdout, captured.clone());
            drain_pipe(stderr, captured.clone());

            // 等孤立读取线程把管道内容写完（短命令通常已结束，此等待为兜底）。
            std::thread::sleep(Duration::from_millis(200));
            let exit_code = unsafe {
                let wait = WaitForSingleObject(handle, INFINITE);
                let mut code: u32 = 0;
                if GetExitCodeProcess(handle, &mut code) == 0 {
                    code = wait;
                }
                CloseHandle(handle);
                code as i32
            };
            Ok::<_, String>((exit_code, drain_captured(captured)))
        })
        .await
        .map_err(|e| format!("PNPM_REPAIR_WAIT: {e}"))??;
        Ok((exit_code, output))
    }

    #[cfg(not(windows))]
    {
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};

        let mut child = Command::new(program)
            .args(args)
            .envs(envs)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0) // 独立进程组，便于将来取消
            .spawn()
            .map_err(|e| format!("PNPM_REPAIR_SPAWN: {e}"))?;
        if let Some(out) = child.stdout.take() {
            drain_pipe(out, captured.clone());
        }
        if let Some(err) = child.stderr.take() {
            drain_pipe(err, captured.clone());
        }
        let exit_code = tauri::async_runtime::spawn_blocking(move || {
            child.wait().map(|s| s.code().unwrap_or(1)).unwrap_or(1)
        })
        .await
        .map_err(|e| format!("PNPM_REPAIR_WAIT: {e}"))?;

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        Ok((exit_code, drain_captured(captured)))
    }
}

/// 在独立线程中把管道读取到 EOF 并追加进共享缓冲区（无事件转发：修复过程
/// 只需落日志，进度反馈非必需）。
fn drain_pipe<R: Read + Send + 'static>(mut reader: R, captured: Arc<Mutex<String>>) {
    std::thread::spawn(move || {
        let mut buffer = Vec::new();
        if reader.read_to_end(&mut buffer).is_err() {
            return;
        }
        if let Ok(mut acc) = captured.lock() {
            acc.push_str(&String::from_utf8_lossy(&buffer));
        }
    });
}

/// 取出（并清空）共享缓冲区中的全部捕获输出。
fn drain_captured(captured: Arc<Mutex<String>>) -> String {
    captured
        .lock()
        .map(|mut buf| std::mem::take(&mut *buf))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn preset(id: &str, package: Option<&str>) -> PreinstallPluginInfo {
        PreinstallPluginInfo {
            id: id.into(),
            spec: String::new(),
            package: package.map(String::from),
            name: String::new(),
            description: String::new(),
            repo_url: String::new(),
            recommended: false,
            fix: false,
            default_checked: false,
            win_only: false,
            internal: false,
        }
    }

    fn setup(dir_label: &str, present: &[&str]) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("dsh-verify-{}-{}", std::process::id(), dir_label));
        let _ = fs::remove_dir_all(&root);
        let node_modules = root.join("node_modules");
        for name in present {
            let dir = node_modules.join(name);
            fs::create_dir_all(&dir).expect("create node_modules pkg dir");
            fs::write(dir.join("package.json"), "{}").expect("write package.json");
        }
        root
    }

    #[test]
    fn missing_detects_absent_but_referenced() {
        let presets = vec![preset("dshmarket", None), preset("dsh-notification", None)];
        let deps = HashMap::from([
            ("dshmarket".to_string(), "1.0.0".to_string()),
            (
                "dsh-notification".to_string(),
                "github:omdsh-dev/dsh-notification".to_string(),
            ),
        ]);
        let bundles = vec!["dshmarket".to_string()];
        // dshmarket 在（引用且产物在）；dsh-notification 被引用但产物缺失
        let root = setup("absent", &["dshmarket"]);
        let missing = missing_plugin_ids(&presets, &deps, &bundles, &root.join("node_modules"));
        assert_eq!(missing, vec!["dsh-notification"]);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn missing_ignores_unreferenced_and_present() {
        let presets = vec![
            preset("dshmarket", None),
            preset("dsh-better-sidebar", None),
        ];
        let deps = HashMap::from([("dshmarket".to_string(), "^1.0.0".to_string())]);
        let bundles: Vec<String> = Vec::new();
        // 全部就绪：dshmarket 已装；better-sidebar 未被引用
        let root = setup("ok", &["dshmarket"]);
        let missing = missing_plugin_ids(&presets, &deps, &bundles, &root.join("node_modules"));
        assert!(missing.is_empty());
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn prune_removes_unavailable_plugin_references() {
        let root = std::env::temp_dir().join(format!(
            "dsh-verify-prune-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root.join("node_modules")).unwrap();
        // 清单引用 3 个社区插件（均缺失产物）+ 官方 bundle（由安装目录提供，不清理）
        fs::write(
            root.join("package.json"),
            r#"{
  "name": "dsh-profile-desktop",
  "dependencies": {
    "dshmarket": "github:omdsh-dev/dshmarket",
    "dsh-win-terminal-inspector": "github:clearkurt/dsh-win-terminal-inspector",
    "@deepseek-ai/dsh-base": "0.1.2-rc.1"
  },
  "dsh": { "profile": { "bundles": ["@deepseek-ai/dsh-base", "dsh-better-sidebar"] } }
}
"#,
        )
        .unwrap();
        // cordis.patch.yml 含 win-terminal-inspector 挂载块（显式入口写法）
        fs::write(
            root.join("cordis.patch.yml"),
            "- insert:\n    - id: win-terminal-inspector\n      name: ./node_modules/dsh-win-terminal-inspector/index.js\n- id: some-row\n  config:\n    a: 1\n",
        )
        .unwrap();

        let pruned = prune_unavailable_plugins_inner(&root).unwrap();
        assert_eq!(pruned.len(), 3);
        assert!(pruned.contains(&"dshmarket".to_string()));
        assert!(pruned.contains(&"dsh-win-terminal-inspector".to_string()));
        assert!(pruned.contains(&"dsh-better-sidebar".to_string()));

        let manifest = fs::read_to_string(root.join("package.json")).unwrap();
        assert!(!manifest.contains("dshmarket"));
        assert!(!manifest.contains("dsh-win-terminal-inspector"));
        assert!(!manifest.contains("dsh-better-sidebar"));
        // 官方 bundle 保留
        assert!(manifest.contains("@deepseek-ai/dsh-base"));

        let patch = fs::read_to_string(root.join("cordis.patch.yml")).unwrap();
        assert!(!patch.contains("win-terminal-inspector"));
        assert!(patch.contains("some-row"));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn prune_keeps_available_plugins() {
        let root = std::env::temp_dir().join(format!(
            "dsh-verify-prune-ok-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        // dshmarket 产物存在
        fs::create_dir_all(root.join("node_modules/dshmarket")).unwrap();
        fs::write(root.join("node_modules/dshmarket/package.json"), "{}").unwrap();
        fs::write(
            root.join("package.json"),
            r#"{
  "dependencies": { "dshmarket": "github:omdsh-dev/dshmarket" },
  "dsh": { "profile": { "bundles": ["dshmarket"] } }
}
"#,
        )
        .unwrap();

        let pruned = prune_unavailable_plugins_inner(&root).unwrap();
        assert!(pruned.is_empty());
        let manifest = fs::read_to_string(root.join("package.json")).unwrap();
        assert!(manifest.contains("dshmarket"));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn missing_resolves_scoped_package_name() {
        // scoped 包名（installed_name 走 package 字段）对应 node_modules/@scope/name
        let presets = vec![preset(
            "dsh-session-context-menu",
            Some("@baihejiangnan/dsh-session-context-menu"),
        )];
        let deps = HashMap::from([(
            "@baihejiangnan/dsh-session-context-menu".to_string(),
            "github:baihejiangnan/dsh-session-context-menu".to_string(),
        )]);
        let bundles = vec!["@baihejiangnan/dsh-session-context-menu".to_string()];
        // 按 scoped 目录探测：存在 → 不缺失
        let root = setup("scoped", &["@baihejiangnan/dsh-session-context-menu"]);
        let missing = missing_plugin_ids(&presets, &deps, &bundles, &root.join("node_modules"));
        assert!(missing.is_empty());
        // 目录缺失 → 缺失
        let missing = missing_plugin_ids(
            &presets,
            &deps,
            &bundles,
            &root.join("missing-node_modules"),
        );
        assert_eq!(missing, vec!["dsh-session-context-menu"]);
        fs::remove_dir_all(&root).ok();
    }
}
