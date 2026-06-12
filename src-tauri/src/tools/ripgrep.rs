use std::path::PathBuf;

const RG_RESOURCE_ROOT: &str = "vendor/rg/bin";

pub(crate) const GREP_MAX_OUTPUT_CHARS: usize = 80 * 1024;
pub(crate) const GREP_MAX_LINE_CHARS: usize = 2 * 1024;

#[derive(Debug, Clone)]
pub(crate) struct RgCommand {
    pub path: PathBuf,
    pub source: RgCommandSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RgCommandSource {
    BundledResource,
    DevVendor,
    Path,
}

pub(crate) fn host_target() -> &'static str {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "aarch64-apple-darwin"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "x86_64-apple-darwin"
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        "x86_64-unknown-linux-gnu"
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        "x86_64-pc-windows-msvc"
    }
    #[cfg(not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "x86_64")
    )))]
    {
        "unsupported"
    }
}

pub(crate) fn executable_name() -> &'static str {
    if cfg!(windows) {
        "rg.exe"
    } else {
        "rg"
    }
}

pub(crate) fn resource_relative_path() -> PathBuf {
    PathBuf::from(RG_RESOURCE_ROOT)
        .join(host_target())
        .join(executable_name())
}

pub(crate) fn dev_vendor_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(resource_relative_path())
}

pub(crate) fn resolve_rg_command(resource_dir: Option<PathBuf>) -> RgCommand {
    if let Some(resource_dir) = resource_dir {
        let candidate = resource_dir.join(resource_relative_path());
        if is_executable_file(&candidate) {
            return RgCommand {
                path: candidate,
                source: RgCommandSource::BundledResource,
            };
        }
    }

    let dev_candidate = dev_vendor_path();
    if is_executable_file(&dev_candidate) {
        return RgCommand {
            path: dev_candidate,
            source: RgCommandSource::DevVendor,
        };
    }

    RgCommand {
        path: PathBuf::from("rg"),
        source: RgCommandSource::Path,
    }
}

fn is_executable_file(path: &std::path::Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_relative_path_uses_host_target() {
        let path = resource_relative_path();
        assert!(path.starts_with(RG_RESOURCE_ROOT));
        assert!(path.to_string_lossy().contains(host_target()));
        assert_eq!(path.file_name().unwrap(), executable_name());
    }

    #[test]
    fn resolve_rg_command_falls_back_to_path_when_no_resource_or_vendor_binary() {
        let command = resolve_rg_command(Some(PathBuf::from("/definitely/missing/resource/dir")));
        if dev_vendor_path().exists() {
            assert_eq!(command.source, RgCommandSource::DevVendor);
        } else {
            assert_eq!(command.source, RgCommandSource::Path);
            assert_eq!(command.path, PathBuf::from("rg"));
        }
    }
}
