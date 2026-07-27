//! Minimal SFTP (SSH File Transfer Protocol) v3 server.
//!
//! Runs over any `AsyncRead + AsyncWrite` stream, typically an SSH channel
//! obtained through `russh::Channel::into_stream`. It implements the subset of
//! the SFTP protocol required by common clients (OpenSSH `sftp`, FileZilla,
//! sshfs, ...): realpath, stat/lstat, opendir/readdir, open/read/write/close,
//! mkdir/rmdir/remove/rename and (best-effort) setstat/symlink.
//!
//! This is intentionally self-contained and does not depend on `russh-sftp`,
//! so it works with the exact `russh` version pinned by this crate.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

// ── SFTP protocol constants ─────────────────────────────────────────

const SFTP_VERSION: u32 = 3;

// request packet types
const SSH_FXP_INIT: u8 = 1;
const SSH_FXP_VERSION: u8 = 2;
const SSH_FXP_OPEN: u8 = 3;
const SSH_FXP_CLOSE: u8 = 4;
const SSH_FXP_READ: u8 = 5;
const SSH_FXP_WRITE: u8 = 6;
const SSH_FXP_LSTAT: u8 = 7;
const SSH_FXP_FSTAT: u8 = 8;
const SSH_FXP_SETSTAT: u8 = 9;
const SSH_FXP_FSETSTAT: u8 = 10;
const SSH_FXP_OPENDIR: u8 = 11;
const SSH_FXP_READDIR: u8 = 12;
const SSH_FXP_REMOVE: u8 = 13;
const SSH_FXP_MKDIR: u8 = 14;
const SSH_FXP_RMDIR: u8 = 15;
const SSH_FXP_REALPATH: u8 = 16;
const SSH_FXP_STAT: u8 = 17;
const SSH_FXP_RENAME: u8 = 18;
const SSH_FXP_READLINK: u8 = 19;
const SSH_FXP_SYMLINK: u8 = 20;

// response packet types
const SSH_FXP_STATUS: u8 = 101;
const SSH_FXP_HANDLE: u8 = 102;
const SSH_FXP_DATA: u8 = 103;
const SSH_FXP_NAME: u8 = 104;
const SSH_FXP_ATTRS: u8 = 105;

// status codes
const FX_OK: u32 = 0;
const FX_EOF: u32 = 1;
const FX_NO_SUCH_FILE: u32 = 2;
const FX_PERMISSION_DENIED: u32 = 3;
const FX_FAILURE: u32 = 4;
const FX_OP_UNSUPPORTED: u32 = 8;

// attribute flags
const ATTR_SIZE: u32 = 0x0000_0001;
const ATTR_UIDGID: u32 = 0x0000_0002;
const ATTR_PERMISSIONS: u32 = 0x0000_0004;
const ATTR_ACMODTIME: u32 = 0x0000_0008;

// file open flags
const OPEN_READ: u32 = 0x0000_0001;
const OPEN_WRITE: u32 = 0x0000_0002;
const OPEN_APPEND: u32 = 0x0000_0004;
const OPEN_CREAT: u32 = 0x0000_0008;
const OPEN_TRUNC: u32 = 0x0000_0010;
const OPEN_EXCL: u32 = 0x0000_0020;

// ── Wire helpers ────────────────────────────────────────────────────

/// A simple big-endian reader over a byte slice.
struct Cur<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cur<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn take_u8(&mut self) -> u8 {
        let v = self.buf[self.pos];
        self.pos += 1;
        v
    }

    fn take_u32(&mut self) -> u32 {
        let b = [
            self.buf[self.pos],
            self.buf[self.pos + 1],
            self.buf[self.pos + 2],
            self.buf[self.pos + 3],
        ];
        self.pos += 4;
        u32::from_be_bytes(b)
    }

    fn take_u64(&mut self) -> u64 {
        let mut b = [0u8; 8];
        b.copy_from_slice(&self.buf[self.pos..self.pos + 8]);
        self.pos += 8;
        u64::from_be_bytes(b)
    }

    fn take_string(&mut self) -> &'a [u8] {
        let n = self.take_u32() as usize;
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        s
    }

    fn take_attrs(&mut self) -> Attrs {
        let flags = self.take_u32();
        let mut a = Attrs::default();
        if flags & ATTR_SIZE != 0 {
            a.size = Some(self.take_u64());
        }
        if flags & ATTR_UIDGID != 0 {
            let _uid = self.take_u32();
            let _gid = self.take_u32();
        }
        if flags & ATTR_PERMISSIONS != 0 {
            a.permissions = Some(self.take_u32());
        }
        if flags & ATTR_ACMODTIME != 0 {
            a.atime = Some(self.take_u32() as u64);
            a.mtime = Some(self.take_u32() as u64);
        }
        a
    }
}

/// On Windows `std::fs::canonicalize`/`read_link` return verbatim paths
/// like `\\?\C:\foo` (or `\\?\UNC\server\share`). Strip the prefix so SFTP
/// clients display a normal path such as `C:\foo`.
fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    const VERBATIM: &str = r"\\?\";
    const UNC: &str = r"\\?\UNC\";
    if let Some(rest) = s.strip_prefix(UNC) {
        PathBuf::from(format!(r"\\{}", rest))
    } else if let Some(rest) = s.strip_prefix(VERBATIM) {
        PathBuf::from(rest)
    } else {
        p
    }
}

/// Format a path the way SFTP clients expect it: the `\\?\` verbatim prefix is
/// removed (see [`strip_verbatim`]) and the separator is normalized to `/`,
/// because the SFTP protocol uses POSIX-style `/` paths regardless of platform.
/// Windows drive paths follow the OpenSSH-on-Windows convention `/C:/foo`
/// (SFTP absolute paths MUST start with `/`; otherwise strict clients such as
/// Bitvise treat them as relative and prepend `/` themselves, breaking
/// round-tripping). The actual filesystem operations still use the real
/// `PathBuf`, only the string sent to the client is converted.
fn to_sftp_path(p: &Path) -> String {
    let s = strip_verbatim(p.to_path_buf()).to_string_lossy().replace('\\', "/");
    let b = s.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        format!("/{}", s)
    } else {
        s
    }
}

/// Convert an SFTP (POSIX-style) path coming from the client into a local
/// filesystem path string. Reverses [`to_sftp_path`] on Windows:
///  - `/C:/foo` (OpenSSH-on-Windows convention) → `C:\foo`
///  - bare drive `C:` → `C:\` (drive root, not a drive-relative path)
fn from_sftp_path(path: &str) -> String {
    #[cfg(windows)]
    {
        let mut s = path.replace('/', "\\");
        let b = s.as_bytes();
        // "\C:..." -> "C:..."
        if b.len() >= 3 && b[0] == b'\\' && b[1].is_ascii_alphabetic() && b[2] == b':' {
            s.remove(0);
        }
        // "C:" -> "C:\"
        let b = s.as_bytes();
        if b.len() == 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
            s.push('\\');
        }
        s
    }
    #[cfg(not(windows))]
    {
        path.to_string()
    }
}

/// A simple big-endian writer backed by a `Vec<u8>`.
struct Pkt {
    buf: Vec<u8>,
}

impl Pkt {
    fn new() -> Self {
        Self { buf: Vec::new() }
    }

    fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    fn string(&mut self, s: &[u8]) {
        self.u32(s.len() as u32);
        self.buf.extend_from_slice(s);
    }
}

/// Frame a single response packet: `[u32 length][u8 type][u32 req_id][payload]`.
fn wire(typ: u8, req_id: u32, payload: &[u8]) -> Vec<u8> {
    let len = 1 + 4 + payload.len();
    let mut out = Vec::with_capacity(4 + len);
    out.extend_from_slice(&(len as u32).to_be_bytes());
    out.push(typ);
    out.extend_from_slice(&req_id.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

#[derive(Default)]
struct Attrs {
    size: Option<u64>,
    #[allow(dead_code)]
    atime: Option<u64>,
    mtime: Option<u64>,
    permissions: Option<u32>,
}

// ── Server state ───────────────────────────────────────────────────

enum SftpHandle {
    File(std::fs::File),
    Dir { entries: Vec<std::fs::DirEntry>, idx: usize },
}

struct Sftp {
    handles: HashMap<String, SftpHandle>,
    next: u32,
    root: PathBuf,
}

impl Sftp {
    fn new() -> Self {
        let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Sftp {
            handles: HashMap::new(),
            next: 1,
            root,
        }
    }

    /// Resolve a client-supplied path. SFTP-style `/C:/foo` paths are first
    /// converted to native form (see [`from_sftp_path`]). Absolute paths are
    /// used verbatim (the management SSH is expected to have filesystem
    /// access); relative paths are resolved against the server's working
    /// directory.
    fn resolve(&self, path: &str) -> PathBuf {
        let native = from_sftp_path(path);
        if native.is_empty() || native == "." {
            return self.root.clone();
        }
        let p = Path::new(&native);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.root.join(&native)
        }
    }

    fn gen_handle(&mut self, h: SftpHandle) -> String {
        let id = self.next;
        self.next += 1;
        let s = format!("h{}", id);
        self.handles.insert(s.clone(), h);
        s
    }

    fn status(&self, req_id: u32, code: u32, msg: &str) -> Vec<u8> {
        let mut p = Pkt::new();
        p.u32(code);
        p.string(msg.as_bytes());
        p.string(b"en-US");
        wire(SSH_FXP_STATUS, req_id, &p.buf)
    }

    fn attrs_packet(&self, req_id: u32, meta: &std::fs::Metadata) -> Vec<u8> {
        wire(SSH_FXP_ATTRS, req_id, &attrs_bytes(meta))
    }

    /// Build a `SSH_FXP_NAME` packet from a list of (filename, display, path).
    fn name_packet(&self, req_id: u32, names: &[(String, String, PathBuf)]) -> Vec<u8> {
        let mut p = Pkt::new();
        p.u32(names.len() as u32);
        for (filename, _display, path) in names {
            p.string(filename.as_bytes());
            let meta = std::fs::symlink_metadata(path).ok();
            let long = match &meta {
                Some(m) => longname(filename, m),
                None => filename.clone(),
            };
            p.string(long.as_bytes());
            let ab = meta.as_ref().map(attrs_bytes).unwrap_or_default();
            p.buf.extend_from_slice(&ab);
        }
        wire(SSH_FXP_NAME, req_id, &p.buf)
    }

    fn handle(&mut self, typ: u8, req_id: u32, payload: &[u8]) -> Vec<Vec<u8>> {
        let mut c = Cur::new(payload);
        match typ {
            SSH_FXP_REALPATH => {
                let path = String::from_utf8_lossy(c.take_string()).to_string();
                let resolved = self.resolve(&path);
                match std::fs::canonicalize(&resolved) {
                    Ok(canon) => {
                        let canon = strip_verbatim(canon);
                        let name = to_sftp_path(&canon);
                        vec![self.name_packet(req_id, &[(name.clone(), name, canon)])]
                    }
                    Err(e) => vec![self.status(req_id, code_for(&e), &e.to_string())],
                }
            }
            SSH_FXP_STAT => self.stat(req_id, &c.take_string().to_vec(), false),
            SSH_FXP_LSTAT => self.stat(req_id, &c.take_string().to_vec(), true),
            SSH_FXP_OPENDIR => {
                let path = String::from_utf8_lossy(c.take_string()).to_string();
                let p = self.resolve(&path);
                match std::fs::read_dir(&p) {
                    Ok(rd) => {
                        let entries: Vec<_> = rd.filter_map(|e| e.ok()).collect();
                        let h = self.gen_handle(SftpHandle::Dir { entries, idx: 0 });
                        let mut pp = Pkt::new();
                        pp.string(h.as_bytes());
                        vec![wire(SSH_FXP_HANDLE, req_id, &pp.buf)]
                    }
                    Err(e) => vec![self.status(req_id, code_for(&e), &e.to_string())],
                }
            }
            SSH_FXP_READDIR => {
                let handle = String::from_utf8_lossy(c.take_string()).to_string();
                let mut responses = Vec::new();
                match self.handles.get_mut(&handle) {
                    Some(SftpHandle::Dir { entries, idx }) => {
                        let mut names: Vec<(String, String, PathBuf)> = Vec::new();
                        while *idx < entries.len() && names.len() < 100 {
                            let entry = &entries[*idx];
                            *idx += 1;
                            if entry.metadata().is_ok() {
                                let fname = entry.file_name().to_string_lossy().to_string();
                                names.push((fname.clone(), fname, entry.path()));
                            }
                        }
                        if names.is_empty() {
                            responses.push(self.status(req_id, FX_EOF, "end of directory"));
                            self.handles.remove(&handle);
                        } else {
                            responses.push(self.name_packet(req_id, &names));
                        }
                    }
                    _ => responses.push(self.status(req_id, FX_FAILURE, "invalid handle")),
                }
                responses
            }
            SSH_FXP_OPEN => {
                let path = String::from_utf8_lossy(c.take_string()).to_string();
                let p = self.resolve(&path);
                let pflags = c.take_u32();
                let _attrs = c.take_attrs();
                let mut opts = std::fs::OpenOptions::new();
                if pflags & OPEN_READ != 0 {
                    opts.read(true);
                }
                if pflags & OPEN_WRITE != 0 {
                    opts.write(true);
                }
                if pflags & OPEN_APPEND != 0 {
                    opts.append(true);
                }
                if pflags & OPEN_CREAT != 0 {
                    opts.create(true);
                }
                if pflags & OPEN_TRUNC != 0 {
                    opts.truncate(true);
                }
                if pflags & OPEN_EXCL != 0 {
                    opts.create_new(true);
                }
                match opts.open(&p) {
                    Ok(f) => {
                        let h = self.gen_handle(SftpHandle::File(f));
                        let mut pp = Pkt::new();
                        pp.string(h.as_bytes());
                        vec![wire(SSH_FXP_HANDLE, req_id, &pp.buf)]
                    }
                    Err(e) => vec![self.status(req_id, code_for(&e), &e.to_string())],
                }
            }
            SSH_FXP_READ => {
                let handle = String::from_utf8_lossy(c.take_string()).to_string();
                let offset = c.take_u64();
                let len = c.take_u32() as usize;
                match self.handles.get_mut(&handle) {
                    Some(SftpHandle::File(f)) => match f.seek(SeekFrom::Start(offset)) {
                        Ok(_) => {
                            let mut buf = vec![0u8; len];
                            match f.read(&mut buf) {
                                Ok(0) => vec![self.status(req_id, FX_EOF, "EOF")],
                                Ok(n) => {
                                    buf.truncate(n);
                                    let mut p = Pkt::new();
                                    p.string(&buf);
                                    vec![wire(SSH_FXP_DATA, req_id, &p.buf)]
                                }
                                Err(e) => vec![self.status(req_id, FX_FAILURE, &e.to_string())],
                            }
                        }
                        Err(e) => vec![self.status(req_id, FX_FAILURE, &e.to_string())],
                    },
                    _ => vec![self.status(req_id, FX_FAILURE, "invalid handle")],
                }
            }
            SSH_FXP_WRITE => {
                let handle = String::from_utf8_lossy(c.take_string()).to_string();
                let offset = c.take_u64();
                let data = c.take_string().to_vec();
                match self.handles.get_mut(&handle) {
                    Some(SftpHandle::File(f)) => match f.seek(SeekFrom::Start(offset)) {
                        Ok(_) => match f.write_all(&data) {
                            Ok(()) => vec![self.status(req_id, FX_OK, "OK")],
                            Err(e) => vec![self.status(req_id, FX_FAILURE, &e.to_string())],
                        },
                        Err(e) => vec![self.status(req_id, FX_FAILURE, &e.to_string())],
                    },
                    _ => vec![self.status(req_id, FX_FAILURE, "invalid handle")],
                }
            }
            SSH_FXP_CLOSE => {
                let handle = String::from_utf8_lossy(c.take_string()).to_string();
                self.handles.remove(&handle);
                vec![self.status(req_id, FX_OK, "OK")]
            }
            SSH_FXP_MKDIR => {
                let path = String::from_utf8_lossy(c.take_string()).to_string();
                let p = self.resolve(&path);
                let _attrs = c.take_attrs();
                match std::fs::create_dir(&p) {
                    Ok(()) => vec![self.status(req_id, FX_OK, "OK")],
                    Err(e) => vec![self.status(req_id, code_for(&e), &e.to_string())],
                }
            }
            SSH_FXP_RMDIR => {
                let path = String::from_utf8_lossy(c.take_string()).to_string();
                let p = self.resolve(&path);
                match std::fs::remove_dir(&p) {
                    Ok(()) => vec![self.status(req_id, FX_OK, "OK")],
                    Err(e) => vec![self.status(req_id, code_for(&e), &e.to_string())],
                }
            }
            SSH_FXP_REMOVE => {
                let path = String::from_utf8_lossy(c.take_string()).to_string();
                let p = self.resolve(&path);
                match std::fs::remove_file(&p) {
                    Ok(()) => vec![self.status(req_id, FX_OK, "OK")],
                    Err(e) => vec![self.status(req_id, code_for(&e), &e.to_string())],
                }
            }
            SSH_FXP_RENAME => {
                let old = String::from_utf8_lossy(c.take_string()).to_string();
                let new = String::from_utf8_lossy(c.take_string()).to_string();
                let op = self.resolve(&old);
                let np = self.resolve(&new);
                match std::fs::rename(&op, &np) {
                    Ok(()) => vec![self.status(req_id, FX_OK, "OK")],
                    Err(e) => vec![self.status(req_id, code_for(&e), &e.to_string())],
                }
            }
            SSH_FXP_SETSTAT => {
                let _path = String::from_utf8_lossy(c.take_string()).to_string();
                let attrs = c.take_attrs();
                if let Some(perm) = attrs.permissions {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let p = self.resolve(&_path);
                        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(perm));
                    }
                    #[cfg(not(unix))]
                    {
                        let _ = perm;
                    }
                }
                vec![self.status(req_id, FX_OK, "OK")]
            }
            SSH_FXP_FSTAT => {
                let handle = String::from_utf8_lossy(c.take_string()).to_string();
                match self.handles.get_mut(&handle) {
                    Some(SftpHandle::File(f)) => match f.metadata() {
                        Ok(meta) => vec![self.attrs_packet(req_id, &meta)],
                        Err(e) => vec![self.status(req_id, FX_FAILURE, &e.to_string())],
                    },
                    _ => vec![self.status(req_id, FX_FAILURE, "invalid handle")],
                }
            }
            SSH_FXP_FSETSTAT => vec![self.status(req_id, FX_OK, "OK")],
            SSH_FXP_READLINK => {
                let path = String::from_utf8_lossy(c.take_string()).to_string();
                let p = self.resolve(&path);
                match std::fs::read_link(&p) {
                    Ok(target) => {
                        let target = strip_verbatim(target);
                        let t = to_sftp_path(&target);
                        vec![self.name_packet(req_id, &[(t.clone(), t, p)])]
                    }
                    Err(e) => vec![self.status(req_id, code_for(&e), &e.to_string())],
                }
            }
            SSH_FXP_SYMLINK => {
                // SFTP v3 order: targetpath first, then linkpath.
                let target = String::from_utf8_lossy(c.take_string()).to_string();
                let link = String::from_utf8_lossy(c.take_string()).to_string();
                let tp = self.resolve(&target);
                let lp = self.resolve(&link);
                #[cfg(unix)]
                let r = std::os::unix::fs::symlink(&tp, &lp);
                #[cfg(windows)]
                let r = std::os::windows::fs::symlink_file(&tp, &lp);
                #[cfg(not(any(unix, windows)))]
                let r = Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "symlink unsupported",
                ));
                match r {
                    Ok(()) => vec![self.status(req_id, FX_OK, "OK")],
                    Err(e) => vec![self.status(req_id, code_for(&e), &e.to_string())],
                }
            }
            _ => vec![self.status(req_id, FX_OP_UNSUPPORTED, "operation unsupported")],
        }
    }

    fn stat(&self, req_id: u32, raw_path: &[u8], follow_links: bool) -> Vec<Vec<u8>> {
        let path = String::from_utf8_lossy(raw_path).to_string();
        let p = self.resolve(&path);
        let res = if follow_links {
            std::fs::metadata(&p)
        } else {
            std::fs::symlink_metadata(&p)
        };
        match res {
            Ok(meta) => vec![self.attrs_packet(req_id, &meta)],
            Err(e) => vec![self.status(req_id, code_for(&e), &e.to_string())],
        }
    }
}

// ── Free helpers ───────────────────────────────────────────────────

fn code_for(e: &std::io::Error) -> u32 {
    match e.kind() {
        std::io::ErrorKind::NotFound => FX_NO_SUCH_FILE,
        std::io::ErrorKind::PermissionDenied => FX_PERMISSION_DENIED,
        _ => FX_FAILURE,
    }
}

fn permission_mode(meta: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode()
    }
    #[cfg(not(unix))]
    {
        if meta.is_dir() {
            0o40_755
        } else if meta.is_symlink() {
            0o120_777
        } else {
            0o100_644
        }
    }
}

fn mode_string(perm: u32) -> String {
    let mut s = String::with_capacity(9);
    for i in (0..3).rev() {
        let bits = (perm >> (i * 3)) & 0o7;
        s.push(if bits & 4 != 0 { 'r' } else { '-' });
        s.push(if bits & 2 != 0 { 'w' } else { '-' });
        s.push(if bits & 1 != 0 { 'x' } else { '-' });
    }
    s
}

fn times(meta: &std::fs::Metadata) -> (u32, u32) {
    let secs = |t: std::io::Result<std::time::SystemTime>| {
        t.ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as u32)
            .unwrap_or(0)
    };
    (secs(meta.accessed()), secs(meta.modified()))
}

fn attrs_bytes(meta: &std::fs::Metadata) -> Vec<u8> {
    let mut p = Pkt::new();
    let perm = permission_mode(meta);
    let size = meta.len();
    let (atime, mtime) = times(meta);
    let flags = ATTR_SIZE | ATTR_PERMISSIONS | ATTR_ACMODTIME;
    p.u32(flags);
    p.u64(size);
    p.u32(perm);
    p.u32(atime);
    p.u32(mtime);
    p.buf
}

fn longname(filename: &str, meta: &std::fs::Metadata) -> String {
    let perm = permission_mode(meta);
    let ftype = if meta.is_dir() {
        'd'
    } else if meta.is_symlink() {
        'l'
    } else {
        '-'
    };
    let mode = mode_string(perm);
    let size = meta.len();
    format!("{} {} 1 user group {} Jan 1 1970 {}", ftype, mode, size, filename)
}

/// Run the SFTP protocol loop over `stream` until the client disconnects.
pub async fn run<S>(mut stream: S) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut sftp = Sftp::new();
    loop {
        let mut len_buf = [0u8; 4];
        match stream.read_exact(&mut len_buf).await {
            Ok(_) => {}
            Err(ref e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e),
        }
        let len = u32::from_be_bytes(len_buf) as usize;
        if len < 1 {
            continue;
        }
        let mut pkt = vec![0u8; len];
        stream.read_exact(&mut pkt).await?;
        let typ = pkt[0];

        // SFTP INIT/VERSION packets do NOT carry a request_id.
        // The first client packet is always SSH_FXP_INIT: [type][u32 version].
        if typ == SSH_FXP_INIT {
            // Reply with SSH_FXP_VERSION (also no request_id):
            //   [u32 length][u8 type][u32 version]
            let mut v = Vec::with_capacity(9);
            v.extend_from_slice(&(5u32).to_be_bytes()); // length of type + version
            v.push(SSH_FXP_VERSION);
            v.extend_from_slice(&SFTP_VERSION.to_be_bytes());
            stream.write_all(&v).await?;
            stream.flush().await?;
            continue;
        }

        // All other packets: [u8 type][u32 request_id][payload…]
        if len < 5 {
            continue; // malformed
        }
        let req_id = u32::from_be_bytes([pkt[1], pkt[2], pkt[3], pkt[4]]);
        let payload = &pkt[5..];
        for resp in sftp.handle(typ, req_id, payload) {
            stream.write_all(&resp).await?;
        }
        stream.flush().await?;
    }
}
