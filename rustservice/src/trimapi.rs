// 调用 fnOS 开放 API（Unix Socket），运行期同步「可访问文件夹」。
//
// 失败必须立刻回退：token/socket/超时都不能拖住启动或列表。
// cmd/main 等 socket 最多约 10s，启动阶段绝不能在这里阻塞。
use crate::config::env_or;
use crate::logf;
use std::io::{Read, Write};
use std::os::fd::FromRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

const SOCK: &str = "/var/run/trim_open_gateway_apiscope.socket";
const RPC_TIMEOUT: Duration = Duration::from_millis(800);

fn token() -> Option<String> {
    std::env::var("TRIM_API_TOKEN").ok().filter(|s| !s.is_empty())
}

fn app_name() -> String {
    env_or("TRIM_APPNAME", "fnnas.pdfreader")
}

fn json_string_array(hay: &str, key: &str) -> Vec<String> {
    let pat = format!("\"{key}\"");
    let Some(i) = hay.find(&pat) else {
        return Vec::new();
    };
    let rest = &hay[i + pat.len()..];
    let Some(lb) = rest.find('[') else {
        return Vec::new();
    };
    let Some(rb) = rest[lb..].find(']') else {
        return Vec::new();
    };
    let inner = &rest[lb + 1..lb + rb];
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_str = false;
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '"' {
            if in_str {
                out.push(std::mem::take(&mut cur));
                in_str = false;
            } else {
                in_str = true;
            }
        } else if in_str {
            if c == '\\' {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            } else {
                cur.push(c);
            }
        }
    }
    out
}

/// Unix 域套接字连接也要有超时。网关 socket 在但没人 accept 时，
/// 阻塞 connect 会一直挂死 spawn_blocking，首页就像打不开。
fn connect_timeout(path: &str, timeout: Duration) -> Option<UnixStream> {
    let bytes = path.as_bytes();
    unsafe {
        let fd = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0);
        if fd < 0 {
            return None;
        }
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 {
            libc::close(fd);
            return None;
        }
        libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);

        let mut addr: libc::sockaddr_un = std::mem::zeroed();
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        if bytes.len() + 1 > addr.sun_path.len() {
            libc::close(fd);
            return None;
        }
        for (i, b) in bytes.iter().enumerate() {
            addr.sun_path[i] = *b as libc::c_char;
        }

        let rc = libc::connect(
            fd,
            &addr as *const _ as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
        );
        if rc != 0 {
            let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            if err != libc::EINPROGRESS && err != libc::EAGAIN {
                libc::close(fd);
                return None;
            }
            let mut pfd = libc::pollfd {
                fd,
                events: libc::POLLOUT,
                revents: 0,
            };
            let ms = timeout.as_millis().min(i32::MAX as u128) as libc::c_int;
            let n = libc::poll(&mut pfd, 1, ms);
            if n <= 0 {
                libc::close(fd);
                return None;
            }
            let mut so_err: libc::c_int = 0;
            let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
            if libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_ERROR,
                &mut so_err as *mut _ as *mut libc::c_void,
                &mut len,
            ) != 0
                || so_err != 0
            {
                libc::close(fd);
                return None;
            }
        }
        libc::fcntl(fd, libc::F_SETFL, flags);
        Some(UnixStream::from_raw_fd(fd))
    }
}

fn rpc(req: &str, data: &str) -> Option<String> {
    if !Path::new(SOCK).exists() {
        return None;
    }
    let token = token()?;
    let app = app_name();
    let body = format!(r#"{{"reqId":"1","req":"{req}","appName":"{app}","data":{data}}}"#);
    let mut s = connect_timeout(SOCK, RPC_TIMEOUT)?;
    let _ = s.set_read_timeout(Some(RPC_TIMEOUT));
    let _ = s.set_write_timeout(Some(RPC_TIMEOUT));
    let http = format!(
        "POST /api/v1/trimapp HTTP/1.1\r\n\
         Host: localhost\r\n\
         Content-Type: application/json\r\n\
         Authorization: Bearer {token}\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );
    s.write_all(http.as_bytes()).ok()?;
    let _ = s.shutdown(std::net::Shutdown::Write);
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let json = match text.find("\r\n\r\n") {
        Some(i) => &text[i + 4..],
        None => text.as_ref(),
    };
    // 只认明确成功，避免 "code":10 一类误伤；失败不打大段响应，免得日志把磁盘打满
    let ok = json.contains("\"code\":0") || json.contains("\"code\": 0");
    if !ok {
        logf!("trim api {req} 失败（回退 roots 文件）");
        return None;
    }
    Some(json.to_string())
}

/// 查询当前授权目录。
///
/// `None`：开放 API 不可用或用户目录查询失败 → 必须回退 roots.txt，不能当成「没有授权」。
/// `Some`：用户目录查询成功（可与共享授权合并）。
pub fn query_accessible_folders(uid: &str) -> Option<Vec<String>> {
    let uid = uid.trim();
    // 启动日志、无登录用户时不要打 RPC
    if uid.is_empty() || uid == "debug" {
        return None;
    }
    if token().is_none() || !Path::new(SOCK).exists() {
        return None;
    }
    let mut out: Vec<String> = Vec::new();
    let mut push = |v: Vec<String>| {
        for p in v {
            if !p.is_empty() && !out.iter().any(|x| x == &p) {
                out.push(p);
            }
        }
    };

    if let Some(j) = rpc("trim.file.getSharedAccessibleFolders", "{}") {
        push(json_string_array(&j, "paths"));
    }

    let data = if let Ok(n) = uid.parse::<i64>() {
        format!(r#"{{"uid":{n}}}"#)
    } else {
        format!(r#"{{"uid":"{}"}}"#, uid.replace('"', ""))
    };
    let j = rpc("trim.file.getUserAccessibleFolders", &data)?;
    push(json_string_array(&j, "paths"));
    Some(out)
}
