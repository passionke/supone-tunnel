// 作者: kejiqing
//! macOS TUN 提权与停内核。
//!
//! 一次管理员授权：给官方 mihomo 和本 helper 都打上 setuid root。
//! 之后启停不再弹密码——停内核走 setuid helper，只杀本壳 data dir 下的 mihomo。

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const HELPER_NAME: &str = "supone-priv";

/// `{work}/bin/supone-priv`
/// Author: kejiqing
pub fn helper_path(work: &Path) -> PathBuf {
    work.join("bin").join(HELPER_NAME)
}

/// CLI 入口（`supone-priv` 二进制）。成功 0，用法错 2，拒绝/失败 1。
/// Author: kejiqing
pub fn main_from_args<I, S>(args: I) -> i32
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let args: Vec<String> = args
        .into_iter()
        .map(|s| s.as_ref().to_string_lossy().into_owned())
        .collect();
    match run_cli(&args) {
        Ok(()) => 0,
        Err(err) => {
            let _ = writeln!(io::stderr(), "{err}");
            if err.starts_with("用法:") {
                2
            } else {
                1
            }
        }
    }
}

fn run_cli(args: &[String]) -> Result<(), String> {
    let mut rest = args.iter().skip(1).map(String::as_str);
    match rest.next() {
        Some("euid") => {
            println!("{}", effective_uid());
            Ok(())
        }
        Some("kill") => {
            let mut port: Option<u16> = None;
            let mut pids: Vec<u32> = Vec::new();
            while let Some(arg) = rest.next() {
                if arg == "--port" {
                    let raw = rest.next().ok_or("用法: supone-priv kill [--port N] [pid...]")?;
                    port = Some(
                        raw.parse()
                            .map_err(|_| format!("无效端口: {raw}"))?,
                    );
                } else if let Ok(pid) = arg.parse::<u32>() {
                    pids.push(pid);
                } else {
                    return Err(format!("用法: supone-priv kill [--port N] [pid...]（未知参数 {arg}）"));
                }
            }
            cli_kill(port, &pids)
        }
        _ => Err("用法: supone-priv euid | kill [--port N] [pid...]".into()),
    }
}

#[cfg(unix)]
fn effective_uid() -> u32 {
    unsafe { libc_geteuid() }
}

#[cfg(unix)]
unsafe extern "C" {
    #[link_name = "geteuid"]
    fn libc_geteuid() -> u32;
}

#[cfg(not(unix))]
fn effective_uid() -> u32 {
    0
}

/// 只根据 helper 自身安装路径推导 work，不信任 argv 里的路径。
/// Author: kejiqing
fn cli_kill(port: Option<u16>, pids: &[u32]) -> Result<(), String> {
    let (work, bin) = trust_paths_from_exe()?;
    let mut targets: Vec<u32> = pids.to_vec();
    if let Some(port) = port {
        targets.extend(listener_pids(port));
    }
    targets.sort_unstable();
    targets.dedup();
    for pid in targets {
        if is_our_mihomo(pid, &bin, &work) {
            force_kill_pid(pid);
        }
    }
    Ok(())
}

/// 平台杀进程：Unix `kill -9`，Windows `taskkill /F`。
/// Author: kejiqing
pub fn force_kill_pid(pid: u32) {
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .output();
    }
    #[cfg(not(windows))]
    {
        let _ = Command::new("kill")
            .args(["-9", &pid.to_string()])
            .status();
    }
}

fn trust_paths_from_exe() -> Result<(PathBuf, PathBuf), String> {
    let exe = std::env::current_exe().map_err(|err| err.to_string())?;
    let exe = exe
        .canonicalize()
        .unwrap_or(exe);
    let name = exe
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    if name != HELPER_NAME {
        return Err(format!("拒绝：可执行文件名必须是 {HELPER_NAME}，当前为 {name}"));
    }
    let bin_dir = exe
        .parent()
        .ok_or("拒绝：无父目录")?;
    if bin_dir.file_name().and_then(|n| n.to_str()) != Some("bin") {
        return Err("拒绝：helper 必须安装在 <work>/bin/".into());
    }
    let work = bin_dir
        .parent()
        .ok_or("拒绝：无 work 目录")?
        .to_path_buf();
    let bin = bin_dir.join(if cfg!(windows) { "mihomo.exe" } else { "mihomo" });
    Ok((work, bin))
}

/// 需要 TUN 时调用：把官方 mihomo + helper 一次性设为 setuid root。
/// Author: kejiqing
#[cfg(target_os = "macos")]
pub fn ensure_tun_privilege(mihomo: &Path, work: &Path) -> Result<(), String> {
    let helper = stage_helper(work).ok();
    let need_mihomo = !has_setuid_root(mihomo);
    let need_helper = helper.as_ref().is_some_and(|path| !helper_ready(path));
    if !need_mihomo && !need_helper {
        return Ok(());
    }
    if need_mihomo && !mihomo.is_file() {
        return Err(format!("内核不存在: {}", mihomo.display()));
    }
    let mut cmds: Vec<String> = Vec::new();
    if need_mihomo {
        let path = shell_single_quote(&mihomo.to_string_lossy());
        cmds.push(format!("chown root:admin {path} && chmod 4755 {path}"));
    }
    if need_helper {
        if let Some(helper) = &helper {
            cmds.push(helper_elevate_shell(helper));
        }
    }
    if cmds.is_empty() {
        return Ok(());
    }
    let joined = cmds.join(" && ");
    let script = format!("do shell script \"{joined}\" with administrator privileges");
    let output = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .map_err(|err| format!("无法请求管理员权限: {err}"))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        let err = if err.trim().is_empty() {
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        } else {
            err.trim().to_string()
        };
        return Err(format!(
            "未获得管理员权限，无法开启 TUN。{}",
            if err.is_empty() {
                "请在密码框点允许。"
            } else {
                err.as_str()
            }
        ));
    }
    if need_mihomo && !has_setuid_root(mihomo) {
        return Err("管理员授权后内核仍无 setuid，TUN 无法开启".into());
    }
    if need_helper {
        if let Some(helper) = &helper {
            if !helper_ready(helper) {
                return Err("管理员授权后停内核 helper 仍无法以 root 运行".into());
            }
        }
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn ensure_tun_privilege(_mihomo: &Path, _work: &Path) -> Result<(), String> {
    Ok(())
}

/// 结束杀不掉的 root 本壳 mihomo：优先 setuid helper（无密码）。
/// 仅当本壳 mihomo 仍活着才回退 osascript——不再因端口残留误弹密码。
/// Author: kejiqing
#[cfg(target_os = "macos")]
pub fn privileged_kill(work: &Path, port: u16, pids: &[u32]) {
    let bin = work.join("bin").join(if cfg!(windows) {
        "mihomo.exe"
    } else {
        "mihomo"
    });
    let _ = try_helper_kill(work, port, pids);
    std::thread::sleep(std::time::Duration::from_millis(150));
    // 本壳 mihomo 已死就绝不弹密码（旧逻辑用 listener_pids 误判，导致每次停止都 osascript）。
    if our_mihomo_still_alive(&bin, work, pids).is_empty() {
        return;
    }
    let _ = stage_helper(work);
    let helper = helper_path(work);
    let known = if pids.is_empty() {
        String::new()
    } else {
        let list = pids
            .iter()
            .map(|pid| pid.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        format!("kill -9 {list}; ")
    };
    let mut script_body = format!(
        "{known}for p in $(lsof -nP -t -iTCP:{port} -sTCP:LISTEN); do kill -9 $p; done"
    );
    if helper.is_file() {
        script_body.push_str("; ");
        script_body.push_str(&helper_elevate_shell(&helper));
    }
    let script = format!(
        "do shell script \"{script_body}; exit 0\" with administrator privileges"
    );
    let _ = Command::new("osascript").arg("-e").arg(script).status();
}

#[cfg(not(target_os = "macos"))]
pub fn privileged_kill(_work: &Path, _port: u16, pids: &[u32]) {
    for pid in pids {
        force_kill_pid(*pid);
    }
}

#[cfg(target_os = "macos")]
fn try_helper_kill(work: &Path, port: u16, pids: &[u32]) -> bool {
    let helper = helper_path(work);
    if !helper_ready(&helper) {
        return false;
    }
    let mut cmd = Command::new(&helper);
    cmd.arg("kill").arg("--port").arg(port.to_string());
    for pid in pids {
        cmd.arg(pid.to_string());
    }
    cmd.status().map(|s| s.success()).unwrap_or(false)
}

fn our_mihomo_still_alive(bin: &Path, work: &Path, pids: &[u32]) -> Vec<u32> {
    let mut left: Vec<u32> = pids
        .iter()
        .copied()
        .filter(|pid| is_our_mihomo(*pid, bin, work))
        .collect();
    left.extend(our_mihomo_pids(bin, work));
    left.sort_unstable();
    left.dedup();
    left
}

fn helper_ready(path: &Path) -> bool {
    has_setuid_root(path) && helper_runs_as_root(path)
}

/// setuid 位在，但 macOS 可能因 xattr/签名忽略提权——以实际 euid 为准。
/// Author: kejiqing
fn helper_runs_as_root(path: &Path) -> bool {
    let Ok(output) = Command::new(path).arg("euid").output() else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    String::from_utf8_lossy(&output.stdout).trim() == "0"
}

fn helper_elevate_shell(helper: &Path) -> String {
    let dest = shell_single_quote(&helper.to_string_lossy());
    if let Some(src) = find_sidecar() {
        let src = shell_single_quote(&src.to_string_lossy());
        // 覆盖旧 helper（清 provenance），再 setuid——否则旧二进制无 euid 子命令会每次误判失败。
        format!(
            "cp -f {src} {dest} && xattr -c {dest} 2>/dev/null; chown root:admin {dest} && chmod 4755 {dest}"
        )
    } else {
        format!(
            "xattr -c {dest} 2>/dev/null; chown root:admin {dest} && chmod 4755 {dest}"
        )
    }
}

/// 把 sidecar 拷到 `{work}/bin/supone-priv`（已可用则不动）。
/// Author: kejiqing
fn stage_helper(work: &Path) -> Result<PathBuf, String> {
    let dest = helper_path(work);
    if helper_ready(&dest) {
        return Ok(dest);
    }
    // 已是 root setuid 但 euid 无效：不能覆盖，留给 osascript 清 xattr 再 chmod。
    if has_setuid_root(&dest) {
        return Ok(dest);
    }
    let src = find_sidecar().ok_or_else(|| {
        format!(
            "未找到 {HELPER_NAME} sidecar（应在应用旁或 target 目录）。请重新构建应用。"
        )
    })?;
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    fs::copy(&src, &dest).map_err(|err| format!("安装停内核 helper 失败: {err}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o755))
            .map_err(|err| err.to_string())?;
    }
    let _ = Command::new("xattr")
        .args(["-c", &dest.to_string_lossy()])
        .status();
    let _ = File::open(&dest).map_err(|err| format!("helper 不可读: {err}"))?;
    Ok(dest)
}

fn find_sidecar() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let path = dir.join(HELPER_NAME);
    if path.is_file() {
        return Some(path);
    }
    None
}

#[cfg(unix)]
pub fn has_setuid_root(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    meta.uid() == 0 && (meta.mode() & 0o4000) != 0
}

#[cfg(not(unix))]
pub fn has_setuid_root(_path: &Path) -> bool {
    false
}

fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

pub fn command_is_our_mihomo(cmd: &str, bin: &Path, work: &Path) -> bool {
    if cmd.is_empty() {
        return false;
    }
    let bin_full = bin.to_string_lossy();
    if !bin_full.is_empty() && path_text_contains(cmd, bin_full.as_ref()) {
        return true;
    }
    let bin_name = bin
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("mihomo");
    let exe = cmd.split_whitespace().next().unwrap_or("");
    let name_ok = exe.eq_ignore_ascii_case(bin_name)
        || exe.ends_with(&format!("/{bin_name}"))
        || exe.ends_with(&format!("\\{bin_name}"))
        || exe.to_ascii_lowercase().ends_with(&format!("\\{}", bin_name.to_ascii_lowercase()));
    name_ok && path_text_contains(cmd, work.to_string_lossy().as_ref())
}

fn path_text_contains(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    if haystack.contains(needle) {
        return true;
    }
    let slash = needle.replace('\\', "/");
    let back = needle.replace('/', "\\");
    haystack.contains(&slash) || haystack.contains(&back)
}

pub fn is_our_mihomo(pid: u32, bin: &Path, work: &Path) -> bool {
    command_is_our_mihomo(&process_command(pid), bin, work)
}

pub fn process_command(pid: u32) -> String {
    #[cfg(windows)]
    {
        windows_process_command(pid)
    }
    #[cfg(not(windows))]
    {
        let output = Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "command="])
            .output();
        match output {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).trim().to_string()
            }
            _ => String::new(),
        }
    }
}

#[cfg(windows)]
fn windows_process_command(pid: u32) -> String {
    let script = format!(
        "(Get-CimInstance Win32_Process -Filter \"ProcessId={pid}\").CommandLine"
    );
    let output = Command::new("powershell")
        .args(["-NoProfile", "-Command", &script])
        .output();
    match output {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }
        _ => String::new(),
    }
}

pub fn our_mihomo_pids(bin: &Path, work: &Path) -> Vec<u32> {
    #[cfg(windows)]
    {
        windows_our_mihomo_pids(bin, work)
    }
    #[cfg(not(windows))]
    {
        let Ok(output) = Command::new("ps").args(["-ax", "-o", "pid=,command="]).output() else {
            return Vec::new();
        };
        if !output.status.success() {
            return Vec::new();
        }
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                let (pid, cmd) = line.split_once(char::is_whitespace)?;
                let pid = pid.trim().parse().ok()?;
                if command_is_our_mihomo(cmd.trim(), bin, work) {
                    Some(pid)
                } else {
                    None
                }
            })
            .collect()
    }
}

#[cfg(windows)]
fn windows_our_mihomo_pids(bin: &Path, work: &Path) -> Vec<u32> {
    let bin_name = bin
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("mihomo.exe");
    let script = format!(
        "Get-CimInstance Win32_Process | Where-Object {{ $_.Name -ieq '{bin_name}' -or $_.Name -ieq 'mihomo.exe' }} | ForEach-Object {{ '{{0}} {{1}}' -f $_.ProcessId, $_.CommandLine }}"
    );
    let Ok(output) = Command::new("powershell")
        .args(["-NoProfile", "-Command", &script])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let (pid, cmd) = line.split_once(char::is_whitespace)?;
            let pid = pid.trim().parse().ok()?;
            if command_is_our_mihomo(cmd.trim(), bin, work) {
                Some(pid)
            } else {
                None
            }
        })
        .collect()
}

pub fn listener_pids(port: u16) -> Vec<u32> {
    #[cfg(windows)]
    {
        windows_listener_pids(port)
    }
    #[cfg(not(windows))]
    {
        let mut lsof = if Path::new("/usr/sbin/lsof").is_file() {
            Command::new("/usr/sbin/lsof")
        } else {
            Command::new("lsof")
        };
        let output = match lsof
            .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-t"])
            .output()
        {
            Ok(output) => output,
            Err(_) => return Vec::new(),
        };
        if !output.status.success() {
            return Vec::new();
        }
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.trim().parse().ok())
            .collect()
    }
}

#[cfg(windows)]
fn windows_listener_pids(port: u16) -> Vec<u32> {
    let Ok(output) = Command::new("netstat").args(["-ano"]).output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    parse_windows_netstat_listeners(&String::from_utf8_lossy(&output.stdout), port)
}

/// 解析 `netstat -ano` 中 LISTENING 且本地端口匹配的 PID。Author: kejiqing
pub(crate) fn parse_windows_netstat_listeners(text: &str, port: u16) -> Vec<u32> {
    let mut pids = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if !line.contains("LISTENING") {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        // TCP  local  foreign  LISTENING  pid
        if parts.len() < 5 {
            continue;
        }
        let local = parts[1];
        let local_port = local
            .rsplit_once(':')
            .and_then(|(_, p)| p.trim_end_matches(']').parse::<u16>().ok());
        if local_port != Some(port) {
            continue;
        }
        if let Ok(pid) = parts[parts.len() - 1].parse::<u32>() {
            pids.push(pid);
        }
    }
    pids.sort_unstable();
    pids.dedup();
    pids
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_mihomo_matches_data_dir_not_verge() {
        let bin = Path::new(
            "/Users/a/Library/Application Support/top.passionke.supone-tunnel/bin/mihomo",
        );
        let work = Path::new("/Users/a/Library/Application Support/top.passionke.supone-tunnel");
        let ours = format!(
            "{} -d {} -f {}/profiles/x/effective.yaml",
            bin.display(),
            work.display(),
            work.display()
        );
        assert!(command_is_our_mihomo(&ours, bin, work));
        let verge = "/Library/Application Support/clash-verge-service/cores/verge-mihomo -d /Library/Application Support/clash-verge-service/users/501/runtime";
        assert!(!command_is_our_mihomo(verge, bin, work));
    }

    #[test]
    fn our_mihomo_matches_windows_backslash_paths() {
        let bin = Path::new(r"C:\Users\a\AppData\Roaming\top.passionke.supone-tunnel\bin\mihomo.exe");
        let work = Path::new(r"C:\Users\a\AppData\Roaming\top.passionke.supone-tunnel");
        let ours = r#"C:\Users\a\AppData\Roaming\top.passionke.supone-tunnel\bin\mihomo.exe -d C:\Users\a\AppData\Roaming\top.passionke.supone-tunnel -f C:\Users\a\AppData\Roaming\top.passionke.supone-tunnel\profiles\x\effective.yaml"#;
        assert!(command_is_our_mihomo(ours, bin, work));
    }

    #[test]
    fn parse_windows_netstat_matches_port() {
        let text = "\
  TCP    0.0.0.0:9090           0.0.0.0:0              LISTENING       4242
  TCP    127.0.0.1:90901        0.0.0.0:0              LISTENING       99
  TCP    [::]:9090              [::]:0                 LISTENING       4242
";
        assert_eq!(parse_windows_netstat_listeners(text, 9090), vec![4242]);
    }

    #[test]
    fn cli_usage_without_subcommand() {
        assert_eq!(main_from_args(["supone-priv"]), 2);
    }

    #[test]
    fn euid_subcommand_prints_number() {
        assert_eq!(main_from_args(["supone-priv", "euid"]), 0);
    }
}
