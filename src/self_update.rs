use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(windows)]
use std::ffi::c_void;
#[cfg(windows)]
use std::mem::size_of;
#[cfg(windows)]
use std::ptr;
#[cfg(windows)]
use windows_sys::Win32::Foundation::{GetLastError, ERROR_INSUFFICIENT_BUFFER};
#[cfg(windows)]
use windows_sys::Win32::Networking::WinHttp::{
    WinHttpAddRequestHeaders, WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest,
    WinHttpQueryDataAvailable, WinHttpQueryHeaders, WinHttpQueryOption, WinHttpReadData,
    WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetOption, WinHttpSetTimeouts,
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_ADDREQ_FLAG_ADD, WINHTTP_ADDREQ_FLAG_REPLACE,
    WINHTTP_FLAG_SECURE, WINHTTP_OPTION_REDIRECT_POLICY, WINHTTP_OPTION_REDIRECT_POLICY_ALWAYS,
    WINHTTP_OPTION_URL, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
};
#[cfg(windows)]
use windows_sys::Win32::UI::Shell::ShellExecuteW;
#[cfg(windows)]
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

const DEFAULT_REPO: &str = "unixwin/niubash";
const USER_AGENT: &str = concat!("niubash/", env!("CARGO_PKG_VERSION"));
const HTTP_TIMEOUT_MS: i32 = 30_000;
const UPDATE_CHECK_TIMEOUT_MS: i32 = 2_500;
const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const INSTALLER_ARGS: [&str; 5] = [
    "/VERYSILENT",
    "/SUPPRESSMSGBOXES",
    "/NORESTART",
    "/CLOSEAPPLICATIONS",
    "/FORCECLOSEAPPLICATIONS",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfUpdateOptions {
    pub check: bool,
    pub dry_run: bool,
    pub force: bool,
    pub repo: String,
}

impl Default for SelfUpdateOptions {
    fn default() -> Self {
        Self {
            check: false,
            dry_run: false,
            force: false,
            repo: DEFAULT_REPO.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GitHubRelease {
    tag_name: String,
    html_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

pub fn run(args: &[String]) -> Result<()> {
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "-h" | "--help"))
    {
        print_self_update_usage();
        return Ok(());
    }

    let options = parse_options(args)?;
    let release = resolve_latest_release(&options.repo, HTTP_TIMEOUT_MS)?;
    let current_tag = format!("v{}", env!("CARGO_PKG_VERSION"));

    if !options.force && !release_is_newer(&release.tag_name, &current_tag) {
        if release.tag_name == current_tag {
            println!("Niubash is already up to date ({current_tag})");
        } else {
            println!(
                "Current Niubash {current_tag} is newer than latest published release {}",
                release.tag_name
            );
        }
        return Ok(());
    }

    let arch = release_arch();
    println!(
        "Latest Niubash release: {} ({})",
        release.tag_name, release.html_url
    );

    let assets = installer_assets(&options.repo, &release.tag_name, arch);
    let asset = assets
        .first()
        .expect("installer_assets always returns at least one asset");

    println!("Installer: {}", asset.name);

    if options.check {
        return Ok(());
    }

    let installer_path = download_first_available_asset(&options.repo, &release.tag_name, &assets)?;
    println!("Downloaded: {}", installer_path.display());

    if options.dry_run {
        return Ok(());
    }

    validate_installer_payload(&installer_path)?;
    launch_installer(&installer_path)?;

    println!("Installer started. Niubash will finish updating after open Niubash sessions close.");
    Ok(())
}

pub fn maybe_print_update_hint() {
    if update_check_disabled() || !update_check_due() {
        return;
    }

    let result = resolve_latest_release(DEFAULT_REPO, UPDATE_CHECK_TIMEOUT_MS);
    match result {
        Ok(release) => {
            let _ = write_update_check_stamp();
            let current_tag = format!("v{}", env!("CARGO_PKG_VERSION"));
            if release_is_newer(&release.tag_name, &current_tag) {
                eprintln!(
                    "niubash: update available: {} (run 'self-update' in the REPL, or 'niubash --self-update' outside it)",
                    release.tag_name
                );
            }
        }
        Err(err) => {
            log::debug!("niubash update check failed: {err}");
            let _ = write_update_check_stamp();
        }
    }
}

fn parse_options(args: &[String]) -> Result<SelfUpdateOptions> {
    let mut options = SelfUpdateOptions::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--check" => options.check = true,
            "--dry-run" => options.dry_run = true,
            "--force" => options.force = true,
            "--repo" => {
                i += 1;
                let Some(repo) = args.get(i) else {
                    anyhow::bail!("--repo requires owner/name");
                };
                options.repo = repo.clone();
            }
            unknown => anyhow::bail!("unknown --self-update option '{}'", unknown),
        }
        i += 1;
    }
    Ok(options)
}

fn print_self_update_usage() {
    println!("Usage: niu --self-update [--check|--dry-run] [--force] [--repo owner/name]");
    println!("       self-update [--check|--dry-run] [--force] [--repo owner/name]");
    println!();
    println!("Inside the Niubash REPL, run `self-update` or `update-niubash`; the current shell exits after handing off the update.");
}

fn validate_installer_payload(path: &Path) -> Result<()> {
    let header =
        std::fs::read(path).with_context(|| format!("read installer {}", path.display()))?;
    if header.len() < 2 || &header[..2] != b"MZ" {
        anyhow::bail!(
            "downloaded installer {} is not a Windows executable",
            path.display()
        );
    }
    Ok(())
}

#[cfg(windows)]
fn active_install_dir() -> Option<PathBuf> {
    install_root_for_executable(&std::env::current_exe().ok()?)
}

#[cfg(windows)]
fn install_root_for_executable(executable: &Path) -> Option<PathBuf> {
    let mut candidate = executable.parent()?.to_path_buf();
    for _ in 0..5 {
        if candidate.join("niu.exe").is_file() && candidate.join("winuxcmd").is_dir() {
            return Some(candidate);
        }
        candidate = candidate.parent()?.to_path_buf();
    }
    None
}

#[cfg(windows)]
fn launch_installer(installer_path: &Path) -> Result<()> {
    let operation = wide_null("open");
    let file = wide_null(&installer_path.to_string_lossy());
    let mut parameters = INSTALLER_ARGS.join(" ");
    if let Some(install_dir) = active_install_dir() {
        // Keep self-update on the installation that launched it. Without an
        // explicit /DIR, Inno Setup may install a second copy under the
        // user's default LocalAppData path and leave this copy unchanged.
        parameters.push_str(" /DIR=\"");
        parameters.push_str(&install_dir.to_string_lossy());
        parameters.push('\"');
        println!("Update target: {}", install_dir.display());
    }
    let parameters = wide_null(&parameters);
    let result = unsafe {
        ShellExecuteW(
            ptr::null_mut(),
            operation.as_ptr(),
            file.as_ptr(),
            parameters.as_ptr(),
            ptr::null(),
            SW_SHOWNORMAL,
        )
    } as isize;

    if result <= 32 {
        anyhow::bail!(
            "start installer {} failed with ShellExecuteW code {}",
            installer_path.display(),
            result
        );
    }

    Ok(())
}

#[cfg(not(windows))]
fn launch_installer(_installer_path: &Path) -> Result<()> {
    anyhow::bail!("self-update installer execution is only supported on Windows")
}

fn resolve_latest_release(repo: &str, timeout_ms: i32) -> Result<GitHubRelease> {
    let url = format!("https://github.com/{repo}/releases/latest");
    let response = http_get(&url, timeout_ms)?;
    let tag_name = latest_tag_from_url(&response.final_url).with_context(|| {
        format!(
            "GitHub latest release redirect did not point at a tag: {}",
            response.final_url
        )
    })?;

    Ok(GitHubRelease {
        tag_name,
        html_url: response.final_url,
    })
}

pub fn resolve_latest_github_release_tag(repo: &str) -> Result<String> {
    Ok(resolve_latest_release(repo, HTTP_TIMEOUT_MS)?.tag_name)
}

pub fn github_release_asset_url(repo: &str, tag: &str, asset_name: &str) -> String {
    format!("https://github.com/{repo}/releases/download/{tag}/{asset_name}")
}

pub fn download_github_release_asset(
    repo: &str,
    tag: &str,
    asset_name: &str,
    cache_dir_name: &str,
) -> Result<PathBuf> {
    let dir = std::env::temp_dir()
        .join(cache_dir_name)
        .join(tag.trim_start_matches('v'));
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let path = dir.join(safe_asset_name(asset_name));
    let url = github_release_asset_url(repo, tag, asset_name);
    let bytes =
        http_get_bytes(&url).with_context(|| format!("download {asset_name} from {url}"))?;
    std::fs::write(&path, bytes)
        .with_context(|| format!("write downloaded release asset {}", path.display()))?;
    Ok(path)
}

fn download_asset(repo: &str, tag: &str, asset: &GitHubAsset) -> Result<PathBuf> {
    let dir = std::env::temp_dir()
        .join("niubash-self-update")
        .join(tag.trim_start_matches('v'));
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let path = dir.join(safe_asset_name(&asset.name));

    let bytes = http_get_bytes(&asset.browser_download_url).with_context(|| {
        format!(
            "download {}/{} from {}",
            repo,
            asset.name,
            tag.trim_start_matches('v')
        )
    })?;
    std::fs::write(&path, bytes)
        .with_context(|| format!("write downloaded installer {}", path.display()))?;
    Ok(path)
}

fn download_first_available_asset(
    repo: &str,
    tag: &str,
    assets: &[GitHubAsset],
) -> Result<PathBuf> {
    let mut errors = Vec::new();
    for asset in assets {
        match download_asset(repo, tag, asset) {
            Ok(path) => return Ok(path),
            Err(err) => errors.push(format!("{}: {err}", asset.name)),
        }
    }

    anyhow::bail!(
        "failed to download installer for {tag}; tried {}",
        errors.join("; ")
    )
}

struct HttpResponse {
    body: Vec<u8>,
    final_url: String,
}

fn http_get_bytes(url: &str) -> Result<Vec<u8>> {
    Ok(http_get(url, HTTP_TIMEOUT_MS)?.body)
}

#[cfg(windows)]
fn http_get(url: &str, timeout_ms: i32) -> Result<HttpResponse> {
    let parsed = ParsedUrl::parse(url)?;
    let user_agent = wide_null(USER_AGENT);
    let host = wide_null(&parsed.host);
    let path = wide_null(&parsed.path);
    let get = wide_null("GET");

    let session = WinHttpHandle::new(
        unsafe {
            WinHttpOpen(
                user_agent.as_ptr(),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                ptr::null(),
                ptr::null(),
                0,
            )
        },
        "WinHttpOpen",
    )?;

    if unsafe {
        WinHttpSetTimeouts(
            session.raw(),
            timeout_ms,
            timeout_ms,
            timeout_ms,
            timeout_ms,
        )
    } == 0
    {
        return Err(winhttp_error("WinHttpSetTimeouts"));
    }

    let connect = WinHttpHandle::new(
        unsafe { WinHttpConnect(session.raw(), host.as_ptr(), parsed.port, 0) },
        "WinHttpConnect",
    )?;

    let flags = if parsed.secure {
        WINHTTP_FLAG_SECURE
    } else {
        0
    };
    let request = WinHttpHandle::new(
        unsafe {
            WinHttpOpenRequest(
                connect.raw(),
                get.as_ptr(),
                path.as_ptr(),
                ptr::null(),
                ptr::null(),
                ptr::null(),
                flags,
            )
        },
        "WinHttpOpenRequest",
    )?;

    let redirect_policy = WINHTTP_OPTION_REDIRECT_POLICY_ALWAYS;
    if unsafe {
        WinHttpSetOption(
            request.raw(),
            WINHTTP_OPTION_REDIRECT_POLICY,
            &redirect_policy as *const u32 as *const c_void,
            size_of::<u32>() as u32,
        )
    } == 0
    {
        return Err(winhttp_error("WinHttpSetOption redirect policy"));
    }

    let headers = wide_null(&request_headers());
    if unsafe {
        WinHttpAddRequestHeaders(
            request.raw(),
            headers.as_ptr(),
            (headers.len() - 1) as u32,
            WINHTTP_ADDREQ_FLAG_ADD | WINHTTP_ADDREQ_FLAG_REPLACE,
        )
    } == 0
    {
        return Err(winhttp_error("WinHttpAddRequestHeaders"));
    }

    if unsafe { WinHttpSendRequest(request.raw(), ptr::null(), 0, ptr::null(), 0, 0, 0) } == 0 {
        return Err(winhttp_error("WinHttpSendRequest"));
    }

    if unsafe { WinHttpReceiveResponse(request.raw(), ptr::null_mut()) } == 0 {
        return Err(winhttp_error("WinHttpReceiveResponse"));
    }

    let status = query_status_code(request.raw())?;
    if !(200..300).contains(&status) {
        let detail = read_response_body(request.raw())
            .map(|body| response_error_detail(&body))
            .unwrap_or_default();
        if detail.is_empty() {
            anyhow::bail!("HTTP status {status} for {url}");
        }
        anyhow::bail!("HTTP status {status} for {url}: {detail}");
    }

    let final_url = query_final_url(request.raw()).unwrap_or_else(|_| url.to_string());
    let body = read_response_body(request.raw())?;
    Ok(HttpResponse { body, final_url })
}

#[cfg(not(windows))]
fn http_get(_url: &str, _timeout_ms: i32) -> Result<HttpResponse> {
    anyhow::bail!("self-update downloads require Windows WinHTTP")
}

#[cfg(windows)]
fn query_status_code(request: *mut c_void) -> Result<u32> {
    let mut status = 0_u32;
    let mut status_size = size_of::<u32>() as u32;
    let mut index = 0_u32;
    if unsafe {
        WinHttpQueryHeaders(
            request,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            ptr::null(),
            &mut status as *mut u32 as *mut c_void,
            &mut status_size,
            &mut index,
        )
    } == 0
    {
        return Err(winhttp_error("WinHttpQueryHeaders status"));
    }
    Ok(status)
}

#[cfg(windows)]
fn read_response_body(request: *mut c_void) -> Result<Vec<u8>> {
    let mut data = Vec::new();

    loop {
        let mut available = 0_u32;
        if unsafe { WinHttpQueryDataAvailable(request, &mut available) } == 0 {
            return Err(winhttp_error("WinHttpQueryDataAvailable"));
        }
        if available == 0 {
            break;
        }

        let old_len = data.len();
        data.resize(old_len + available as usize, 0);

        let mut consumed = 0_u32;
        while consumed < available {
            let mut read = 0_u32;
            if unsafe {
                WinHttpReadData(
                    request,
                    data[old_len + consumed as usize..].as_mut_ptr() as *mut c_void,
                    available - consumed,
                    &mut read,
                )
            } == 0
            {
                return Err(winhttp_error("WinHttpReadData"));
            }
            if read == 0 {
                break;
            }
            consumed += read;
        }

        data.truncate(old_len + consumed as usize);
        if consumed == 0 {
            break;
        }
    }

    Ok(data)
}

#[cfg(windows)]
fn query_final_url(request: *mut c_void) -> Result<String> {
    let mut size = 0_u32;
    if unsafe { WinHttpQueryOption(request, WINHTTP_OPTION_URL, ptr::null_mut(), &mut size) } != 0 {
        return Ok(String::new());
    }
    let error = unsafe { GetLastError() };
    if error != ERROR_INSUFFICIENT_BUFFER || size == 0 {
        return Err(winhttp_error("WinHttpQueryOption URL"));
    }

    let mut buffer = vec![0_u16; (size as usize + 1) / 2];
    if unsafe {
        WinHttpQueryOption(
            request,
            WINHTTP_OPTION_URL,
            buffer.as_mut_ptr() as *mut c_void,
            &mut size,
        )
    } == 0
    {
        return Err(winhttp_error("WinHttpQueryOption URL"));
    }

    let len = (size as usize) / 2;
    buffer.truncate(len);
    while buffer.last() == Some(&0) {
        buffer.pop();
    }
    Ok(String::from_utf16_lossy(&buffer))
}

#[cfg(windows)]
#[derive(Debug)]
struct ParsedUrl {
    secure: bool,
    host: String,
    port: u16,
    path: String,
}

#[cfg(windows)]
impl ParsedUrl {
    fn parse(url: &str) -> Result<Self> {
        let (secure, rest) = if let Some(rest) = url.strip_prefix("https://") {
            (true, rest)
        } else if let Some(rest) = url.strip_prefix("http://") {
            (false, rest)
        } else {
            anyhow::bail!("unsupported URL scheme in {url}");
        };

        let split_at = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let authority = &rest[..split_at];
        let path_tail = &rest[split_at..];
        let path_tail = path_tail
            .split_once('#')
            .map(|(path, _fragment)| path)
            .unwrap_or(path_tail);
        if authority.is_empty() || authority.contains('@') {
            anyhow::bail!("unsupported URL authority in {url}");
        }

        let (host, port) = split_host_port(authority, secure)?;
        let path = if path_tail.is_empty() {
            "/".to_string()
        } else if path_tail.starts_with('/') {
            path_tail.to_string()
        } else {
            format!("/{path_tail}")
        };

        Ok(Self {
            secure,
            host,
            port,
            path,
        })
    }
}

#[cfg(windows)]
fn split_host_port(authority: &str, secure: bool) -> Result<(String, u16)> {
    let default_port = if secure { 443 } else { 80 };

    if authority.starts_with('[') {
        let Some(end) = authority.find(']') else {
            anyhow::bail!("invalid IPv6 URL host");
        };
        let host = authority[..=end].to_string();
        let rest = &authority[end + 1..];
        let port = if let Some(raw) = rest.strip_prefix(':') {
            parse_port(raw)?
        } else if rest.is_empty() {
            default_port
        } else {
            anyhow::bail!("invalid URL authority");
        };
        return Ok((host, port));
    }

    if let Some((host, raw_port)) = authority.rsplit_once(':') {
        if raw_port.is_empty() || !raw_port.chars().all(|ch| ch.is_ascii_digit()) {
            anyhow::bail!("invalid URL port '{raw_port}'");
        }
        if host.is_empty() {
            anyhow::bail!("invalid URL host");
        }
        return Ok((host.to_string(), parse_port(raw_port)?));
    }

    Ok((authority.to_string(), default_port))
}

#[cfg(windows)]
fn parse_port(raw: &str) -> Result<u16> {
    raw.parse::<u16>()
        .with_context(|| format!("invalid URL port '{raw}'"))
}

#[cfg(windows)]
fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn request_headers() -> String {
    request_headers_for_token(github_token().as_deref())
}

fn request_headers_for_token(token: Option<&str>) -> String {
    let mut headers = format!(
        "User-Agent: {USER_AGENT}\r\nAccept: application/octet-stream, text/html;q=0.9, */*;q=0.8\r\n"
    );
    if let Some(token) = token.and_then(clean_header_value) {
        headers.push_str("Authorization: Bearer ");
        headers.push_str(token);
        headers.push_str("\r\n");
    }
    headers
}

fn latest_tag_from_url(url: &str) -> Option<String> {
    let without_fragment = url.split('#').next().unwrap_or(url);
    let without_query = without_fragment
        .split('?')
        .next()
        .unwrap_or(without_fragment);
    let marker = "/releases/tag/";
    let tag = without_query.split(marker).nth(1)?;
    let tag = tag.split('/').next().unwrap_or(tag);
    if parse_version_tag(tag).is_some() {
        Some(tag.to_string())
    } else {
        None
    }
}

fn installer_assets(repo: &str, tag: &str, arch: &str) -> Vec<GitHubAsset> {
    let stable_name = format!("niubash-win-{arch}-setup.exe");
    vec![
        GitHubAsset {
            name: stable_name.clone(),
            browser_download_url: format!(
                "https://github.com/{repo}/releases/latest/download/{stable_name}"
            ),
        },
        versioned_installer_asset(repo, tag, arch),
    ]
}

fn versioned_installer_asset(repo: &str, tag: &str, arch: &str) -> GitHubAsset {
    let version = tag.trim_start_matches('v');
    let name = format!("niubash-v{version}-win-{arch}-setup.exe");
    GitHubAsset {
        name: name.clone(),
        browser_download_url: github_release_asset_url(repo, tag, &name),
    }
}

fn update_check_disabled() -> bool {
    env_flag_is_disabled("NIU_UPDATE_CHECK")
        || env_flag_is_enabled("NIU_NO_UPDATE_CHECK")
        || std::env::var_os("CI").is_some()
}

fn env_flag_is_disabled(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
        .unwrap_or(false)
}

fn env_flag_is_enabled(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn update_check_due() -> bool {
    let Ok(metadata) = std::fs::metadata(update_check_stamp_path()) else {
        return true;
    };
    let Ok(modified) = metadata.modified() else {
        return true;
    };
    modified
        .elapsed()
        .map(|elapsed| elapsed >= UPDATE_CHECK_INTERVAL)
        .unwrap_or(true)
}

fn write_update_check_stamp() -> Result<()> {
    let path = update_check_stamp_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    std::fs::write(&path, format!("{now}\n")).with_context(|| format!("write {}", path.display()))
}

fn update_check_stamp_path() -> PathBuf {
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(local_app_data)
            .join("Niubash")
            .join("update-check.stamp");
    }
    std::env::temp_dir()
        .join("niubash")
        .join("update-check.stamp")
}

fn github_token() -> Option<String> {
    ["GH_TOKEN", "GITHUB_TOKEN"]
        .into_iter()
        .filter_map(|key| std::env::var(key).ok())
        .find(|value| clean_header_value(value).is_some())
}

fn clean_header_value(value: &str) -> Option<&str> {
    let value = value.trim();
    if value.is_empty() || value.contains(['\r', '\n']) {
        None
    } else {
        Some(value)
    }
}

fn response_error_detail(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(3)
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(300)
        .collect()
}

#[cfg(windows)]
fn winhttp_error(action: &str) -> anyhow::Error {
    let code = unsafe { GetLastError() };
    anyhow::anyhow!("{action} failed with Windows error {code}")
}

#[cfg(windows)]
struct WinHttpHandle(*mut c_void);

#[cfg(windows)]
impl WinHttpHandle {
    fn new(raw: *mut c_void, action: &str) -> Result<Self> {
        if raw.is_null() {
            Err(winhttp_error(action))
        } else {
            Ok(Self(raw))
        }
    }

    fn raw(&self) -> *mut c_void {
        self.0
    }
}

#[cfg(windows)]
impl Drop for WinHttpHandle {
    fn drop(&mut self) {
        unsafe {
            WinHttpCloseHandle(self.0);
        }
    }
}

fn safe_asset_name(name: &str) -> String {
    name.chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' | '_' => ch,
            _ => '_',
        })
        .collect()
}

fn release_arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        _ => "x64",
    }
}

fn release_is_newer(release_tag: &str, current_tag: &str) -> bool {
    let Some(release) = parse_version_tag(release_tag) else {
        return release_tag != current_tag;
    };
    let Some(current) = parse_version_tag(current_tag) else {
        return true;
    };
    release > current
}

fn parse_version_tag(tag: &str) -> Option<(u64, u64, u64)> {
    let tag = tag.strip_prefix('v').unwrap_or(tag);
    let mut parts = tag.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_versioned_installer_asset() {
        let asset = versioned_installer_asset("unixwin/niubash", "v0.8.2", "x64");

        assert_eq!(asset.name, "niubash-v0.8.2-win-x64-setup.exe");
        assert_eq!(
            asset.browser_download_url,
            "https://github.com/unixwin/niubash/releases/download/v0.8.2/niubash-v0.8.2-win-x64-setup.exe"
        );
    }

    #[test]
    fn builds_stable_installer_asset_before_versioned_fallback() {
        let assets = installer_assets("unixwin/niubash", "v0.8.2", "x64");

        assert_eq!(assets[0].name, "niubash-win-x64-setup.exe");
        assert_eq!(
            assets[0].browser_download_url,
            "https://github.com/unixwin/niubash/releases/latest/download/niubash-win-x64-setup.exe"
        );
        assert_eq!(assets[1].name, "niubash-v0.8.2-win-x64-setup.exe");
    }

    #[test]
    fn builds_generic_github_release_asset_url() {
        assert_eq!(
            github_release_asset_url(
                "unixwin/oh-my-winuxsh",
                "v1.0.0",
                "oh-my-winuxsh-1.0.0.zip"
            ),
            "https://github.com/unixwin/oh-my-winuxsh/releases/download/v1.0.0/oh-my-winuxsh-1.0.0.zip"
        );
    }

    #[test]
    fn sanitizes_download_file_names() {
        assert_eq!(safe_asset_name("../bad setup.exe"), ".._bad_setup.exe");
    }

    #[test]
    fn installer_args_keep_silent_forced_close_contract() {
        assert!(INSTALLER_ARGS.contains(&"/VERYSILENT"));
        assert!(INSTALLER_ARGS.contains(&"/CLOSEAPPLICATIONS"));
        assert!(INSTALLER_ARGS.contains(&"/FORCECLOSEAPPLICATIONS"));
    }

    #[test]
    fn accepts_windows_executable_installer_payload() {
        let path = temp_installer_path("valid");
        std::fs::write(&path, b"MZfake").unwrap();

        validate_installer_payload(&path).unwrap();

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn rejects_non_executable_installer_payload() {
        let path = temp_installer_path("invalid");
        std::fs::write(&path, b"<!doctype html>").unwrap();

        let err = validate_installer_payload(&path).unwrap_err().to_string();

        assert!(err.contains("not a Windows executable"), "{err}");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn request_headers_include_generic_download_defaults() {
        let headers = request_headers_for_token(None);

        assert!(headers.contains("User-Agent: niubash/"));
        assert!(headers.contains("Accept: application/octet-stream"));
        assert!(!headers.contains("Authorization:"));
    }

    #[test]
    fn request_headers_can_include_clean_github_token() {
        let headers = request_headers_for_token(Some("  ghp_test  "));

        assert!(headers.contains("Authorization: Bearer ghp_test\r\n"));
    }

    #[test]
    fn request_headers_skip_header_injection_tokens() {
        let headers = request_headers_for_token(Some("good\r\nX-Bad: yes"));

        assert!(!headers.contains("Authorization:"));
    }

    #[test]
    fn response_error_detail_trims_and_limits_output() {
        let detail = response_error_detail(b"\n first line \nsecond line\nthird line\nfourth line");

        assert_eq!(detail, "first line second line third line");
    }

    #[test]
    fn extracts_latest_release_tag_from_redirect_url() {
        assert_eq!(
            latest_tag_from_url("https://github.com/unixwin/niubash/releases/tag/v0.8.2"),
            Some("v0.8.2".to_string())
        );
        assert_eq!(
            latest_tag_from_url(
                "https://github.com/unixwin/niubash/releases/tag/v0.8.2?expanded=true"
            ),
            Some("v0.8.2".to_string())
        );
        assert_eq!(
            latest_tag_from_url("https://github.com/unixwin/niubash/releases/latest"),
            None
        );
    }

    #[cfg(windows)]
    #[test]
    fn finds_install_root_from_winuxcmd_shim_path() {
        let root = std::env::temp_dir().join(format!(
            "niubash-self-update-root-test-{}",
            std::process::id()
        ));
        let shim = root.join("winuxcmd").join("bin").join("sh.exe");
        std::fs::create_dir_all(shim.parent().unwrap()).unwrap();
        std::fs::create_dir_all(root.join("winuxcmd")).unwrap();
        std::fs::write(root.join("niu.exe"), b"MZ").unwrap();
        std::fs::write(&shim, b"MZ").unwrap();

        assert_eq!(install_root_for_executable(&shim), Some(root.clone()));
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn parses_https_url_for_winhttp() {
        let parsed =
            ParsedUrl::parse("https://api.github.com/repos/unixwin/niubash/releases/latest?x=1")
                .unwrap();

        assert!(parsed.secure);
        assert_eq!(parsed.host, "api.github.com");
        assert_eq!(parsed.port, 443);
        assert_eq!(parsed.path, "/repos/unixwin/niubash/releases/latest?x=1");
    }

    #[cfg(windows)]
    #[test]
    fn strips_url_fragment_before_winhttp_request() {
        let parsed = ParsedUrl::parse("https://example.com/downloads/app.exe?x=1#asset").unwrap();

        assert_eq!(parsed.host, "example.com");
        assert_eq!(parsed.path, "/downloads/app.exe?x=1");
    }

    #[cfg(windows)]
    #[test]
    fn rejects_invalid_url_port() {
        assert!(ParsedUrl::parse("https://example.com:not-a-port/file").is_err());
    }

    #[test]
    fn compares_release_tags_without_downgrading() {
        assert!(release_is_newer("v0.8.2", "v0.8.1"));
        assert!(!release_is_newer("v0.8.1", "v0.8.1"));
        assert!(!release_is_newer("v0.8.0", "v0.8.1"));
    }

    fn temp_installer_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "niubash-{name}-installer-{}-{nanos}.exe",
            std::process::id()
        ))
    }
}
