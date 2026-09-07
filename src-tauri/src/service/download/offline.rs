//! 安装包内置离线资源（Node / dsh / pnpm / MinGit）查找。
//!
//! 离线资源随安装包分发（`resources/offline/`），存在时安装/自愈流程直接
//! 从资源解压，不联网下载；未内置对应平台资源时回退到网络下载（原逻辑），
//! 因此未提供离线资源的平台（如 macOS / Linux 构建）行为完全不变。

use crate::config;
use crate::service::download::InstallKind;
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

/// 离线资源目录名（相对 resource_dir）
const OFFLINE_DIR: &str = "offline";

/// 查找安装包内置的离线资源文件。
///
/// Tauri 打包 `resources/**/*` 时保留相对路径，resource_dir 下可能直接是
/// `offline/<file>`（开发构建）或 `resources/offline/<file>`（安装布局），
/// 两种候选都尝试。
pub fn offline_resource_path(app_handle: &AppHandle, filename: &str) -> Option<PathBuf> {
    let root = app_handle.path().resource_dir().ok()?;
    let candidates = [
        root.join(OFFLINE_DIR).join(filename),
        root.join("resources").join(OFFLINE_DIR).join(filename),
    ];
    candidates.into_iter().find(|p| p.is_file())
}

/// 离线资源文件名（按当前平台与架构生成）。
///
/// 与 `config::runtime` 中网络下载的文件名保持一致；未内置离线资源的平台
/// 返回 `None`，调用方回退到网络下载。
pub fn offline_filename(kind: InstallKind) -> Option<String> {
    offline_filename_for(kind, std::env::consts::OS, std::env::consts::ARCH)
}

/// 纯函数版本：按指定平台/架构生成离线资源文件名（便于跨平台单元测试）。
fn offline_filename_for(kind: InstallKind, os: &str, arch: &str) -> Option<String> {
    match kind {
        InstallKind::Node => match (os, arch) {
            ("windows", _) => Some(format!("node-{}-win-x64.zip", config::NODE_VERSION)),
            ("macos", "aarch64") => {
                Some(format!("node-{}-darwin-arm64.tar.gz", config::NODE_VERSION))
            }
            ("macos", "x86_64") => {
                Some(format!("node-{}-darwin-x64.tar.gz", config::NODE_VERSION))
            }
            ("linux", "x86_64") => {
                Some(format!("node-{}-linux-x64.tar.gz", config::NODE_VERSION))
            }
            ("linux", "aarch64") => {
                Some(format!("node-{}-linux-arm64.tar.gz", config::NODE_VERSION))
            }
            _ => None,
        },
        InstallKind::Dsh => match (os, arch) {
            ("windows", _) => Some("deepseek-harness-pkg-windows.zip".to_string()),
            ("macos", "aarch64") => Some("deepseek-harness-pkg-macos-arm64.zip".to_string()),
            ("macos", "x86_64") => Some("deepseek-harness-pkg-macos-x64.zip".to_string()),
            ("linux", _) => Some("deepseek-harness-pkg-linux.zip".to_string()),
            _ => None,
        },
        InstallKind::Pnpm => Some(format!("pnpm-{}.tgz", config::PNPM_VERSION)),
        InstallKind::Git => match (os, arch) {
            ("windows", "x86_64") => {
                Some(format!("MinGit-{}-64-bit.zip", config::MINGIT_VERSION))
            }
            ("windows", "aarch64") => {
                Some(format!("MinGit-{}-arm64.zip", config::MINGIT_VERSION))
            }
            _ => None,
        },
    }
}

/// 当前平台是否内置了 dsh 离线资源（存在则安装/自愈不联网核对版本）。
pub fn dsh_offline_available(app_handle: &AppHandle) -> bool {
    offline_filename(InstallKind::Dsh)
        .and_then(|name| offline_resource_path(app_handle, &name))
        .is_some()
}

#[cfg(test)]
mod tests {
    use super::offline_filename_for;
    use crate::service::download::InstallKind;

    #[test]
    fn windows_offline_filenames_match_runtime() {
        // 与 config::runtime 的网络下载文件名保持一致（Windows x64 基准）
        let node = offline_filename_for(InstallKind::Node, "windows", "x86_64");
        assert_eq!(
            node.as_deref(),
            Some(format!("node-{}-win-x64.zip", crate::config::NODE_VERSION).as_str())
        );
        let dsh = offline_filename_for(InstallKind::Dsh, "windows", "x86_64");
        assert_eq!(dsh.as_deref(), Some("deepseek-harness-pkg-windows.zip"));
        let pnpm = offline_filename_for(InstallKind::Pnpm, "windows", "x86_64");
        assert_eq!(
            pnpm.as_deref(),
            Some(format!("pnpm-{}.tgz", crate::config::PNPM_VERSION).as_str())
        );
        let git = offline_filename_for(InstallKind::Git, "windows", "x86_64");
        assert_eq!(
            git.as_deref(),
            Some(format!("MinGit-{}-64-bit.zip", crate::config::MINGIT_VERSION).as_str())
        );
    }

    #[test]
    fn unsupported_platform_returns_none() {
        // Windows 任意架构的 Node 均使用 win-x64 包（与 config::runtime 一致）
        assert_eq!(
            offline_filename_for(InstallKind::Node, "windows", "arm64").as_deref(),
            Some(format!("node-{}-win-x64.zip", crate::config::NODE_VERSION).as_str())
        );
        // Git 仅 Windows 有离线资源
        assert_eq!(offline_filename_for(InstallKind::Git, "linux", "x86_64"), None);
        assert_eq!(offline_filename_for(InstallKind::Git, "macos", "aarch64"), None);
    }
}
