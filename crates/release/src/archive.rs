//! Safe extraction of release archives, with the installers' rules
//! (`install-metro-linux.sh`, `install-metro-windows.ps1`): regular files
//! only, every entry under the exact top folder, no `..`/`.`/absolute paths,
//! backslashes or colons, size and entry caps, and a `RELEASE.json` whose
//! kind, target, version and exact file set (with hashes) match.
//!
//! The readers are deliberately narrow: `package.py` writes USTAR tar.gz
//! (regular files only) and ZIPs of stored/deflated regular files without
//! ZIP64. Anything else is refused rather than interpreted.
//!
//! Callers pass a fresh, empty destination folder they created on the same
//! filesystem as the final location. Extraction never follows or creates a
//! link, so nothing outside it can be written.

use crate::error::{Error, ErrorCode, require};
use crate::version::Version;
use crate::{MAX_ARCHIVE, hex, parse_hex32};
use sha2::Digest;
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const MAX_ENTRIES: usize = 20_000;
const MAX_RELEASE_JSON: u64 = 64 * 1024;
pub const RELEASE_JSON: &str = "RELEASE.json";

/// What the archive must say about itself.
#[derive(Clone, Copy, Debug)]
pub struct Expect<'a> {
    pub kind: &'a str,
    pub target: &'a str,
    pub version: &'a Version,
}

/// A verified, extracted tree.
#[derive(Clone, Debug)]
pub struct VerifiedTree {
    pub root: PathBuf,
    /// Relative path → SHA-256 (as `RELEASE.json` "files").
    pub files: BTreeMap<String, [u8; 32]>,
    /// The whole parsed `RELEASE.json`.
    pub release: serde_json::Value,
}

fn archive_error(detail: &str) -> Error {
    Error::new(ErrorCode::Archive, detail)
}

fn io_error(context: &str, error: io::Error) -> Error {
    Error::new(ErrorCode::Io, format!("{context}: {}", error.kind()))
}

/// The relative path under `top/`, if the entry name is acceptable.
fn relative<'n>(name: &'n str, top: &str) -> Result<&'n str, Error> {
    let rel = name
        .strip_prefix(top)
        .and_then(|r| r.strip_prefix('/'))
        .ok_or_else(|| archive_error("entry outside the release folder"))?;
    require(
        !rel.is_empty() && rel.len() <= 1024,
        ErrorCode::Archive,
        "bad entry path",
    )?;
    for part in rel.split('/') {
        require(
            !part.is_empty()
                && part != "."
                && part != ".."
                && part.len() <= 255
                && !part.bytes().any(|c| {
                    c < 0x20
                        || c == 0x7f
                        || matches!(c, b'\\' | b':' | b'*' | b'?' | b'"' | b'<' | b'>' | b'|')
                }),
            ErrorCode::Archive,
            "bad entry path",
        )?;
    }
    Ok(rel)
}

struct Sink<'a> {
    dest: &'a Path,
    top: &'a str,
    files: BTreeMap<String, [u8; 32]>,
    release_json: Option<Vec<u8>>,
    total: u64,
}

impl<'a> Sink<'a> {
    fn new(dest: &'a Path, top: &'a str) -> Result<Self, Error> {
        let mut entries = fs::read_dir(dest).map_err(|e| io_error("staging folder", e))?;
        require(
            entries.next().is_none(),
            ErrorCode::Io,
            "staging folder isn't empty",
        )?;
        Ok(Sink {
            dest,
            top,
            files: BTreeMap::new(),
            release_json: None,
            total: 0,
        })
    }

    fn file(
        &mut self,
        name: &str,
        size: u64,
        executable: bool,
        data: &mut dyn Read,
    ) -> Result<(), Error> {
        let rel = relative(name, self.top)?.to_string();
        require(
            self.files.len() < MAX_ENTRIES,
            ErrorCode::TooLarge,
            "archive has too many entries",
        )?;
        self.total = self.total.saturating_add(size);
        require(
            self.total <= MAX_ARCHIVE,
            ErrorCode::TooLarge,
            "archive is too large",
        )?;
        require(
            !self.files.contains_key(&rel),
            ErrorCode::Archive,
            "archive repeats an entry",
        )?;
        let path = rel
            .split('/')
            .fold(self.dest.to_path_buf(), |p, part| p.join(part));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| io_error("creating a folder", e))?;
        }
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(if executable { 0o755 } else { 0o644 });
        }
        #[cfg(not(unix))]
        let _ = executable;
        let mut out = options.open(&path).map_err(|e| {
            if e.kind() == io::ErrorKind::AlreadyExists {
                archive_error("archive repeats an entry")
            } else {
                io_error("writing a file", e)
            }
        })?;
        let keep = rel == RELEASE_JSON;
        if keep {
            require(
                size <= MAX_RELEASE_JSON,
                ErrorCode::Archive,
                "RELEASE.json is too large",
            )?;
        }
        let mut kept = Vec::new();
        let mut hasher = sha2::Sha256::new();
        let mut remaining = size;
        let mut buf = vec![0u8; 64 * 1024];
        while remaining > 0 {
            let want = remaining.min(buf.len() as u64) as usize;
            let got = data
                .read(&mut buf[..want])
                .map_err(|_| archive_error("archive data is truncated or corrupt"))?;
            require(got > 0, ErrorCode::Archive, "archive data is truncated")?;
            hasher.update(&buf[..got]);
            out.write_all(&buf[..got])
                .map_err(|e| io_error("writing a file", e))?;
            if keep {
                kept.extend_from_slice(&buf[..got]);
            }
            remaining -= got as u64;
        }
        out.sync_all().map_err(|e| io_error("syncing a file", e))?;
        if keep {
            self.release_json = Some(kept);
        }
        self.files.insert(rel, hasher.finalize().into());
        Ok(())
    }

    fn finish(self, expect: &Expect) -> Result<VerifiedTree, Error> {
        let raw = self
            .release_json
            .ok_or_else(|| archive_error("archive lacks RELEASE.json"))?;
        let release: serde_json::Value =
            serde_json::from_slice(&raw).map_err(|_| archive_error("RELEASE.json is not JSON"))?;
        let field = |k: &str| release.get(k).and_then(|v| v.as_str());
        let version = expect.version.to_string();
        require(
            field("kind") == Some(expect.kind),
            ErrorCode::Archive,
            "RELEASE.json is for another kind",
        )?;
        require(
            field("target") == Some(expect.target),
            ErrorCode::Archive,
            "RELEASE.json is for another target",
        )?;
        require(
            field("version") == Some(version.as_str()),
            ErrorCode::Archive,
            "RELEASE.json is for another version",
        )?;
        let listed = release
            .get("files")
            .and_then(|f| f.as_object())
            .ok_or_else(|| archive_error("RELEASE.json lacks its file list"))?;
        let mut expected = BTreeMap::new();
        for (name, digest) in listed {
            let digest = digest
                .as_str()
                .and_then(parse_hex32)
                .ok_or_else(|| archive_error("RELEASE.json has a bad file hash"))?;
            expected.insert(name.clone(), digest);
        }
        let mut have = self.files;
        have.remove(RELEASE_JSON);
        for (name, digest) in &have {
            match expected.get(name) {
                None => {
                    return Err(archive_error(
                        "archive has a file RELEASE.json doesn't list",
                    ));
                }
                Some(want) if want != digest => {
                    return Err(Error::new(
                        ErrorCode::NotVerified,
                        format!("file hash mismatch: {name}"),
                    ));
                }
                Some(_) => {}
            }
        }
        require(
            have.len() == expected.len(),
            ErrorCode::Archive,
            "archive lacks a file RELEASE.json lists",
        )?;
        Ok(VerifiedTree {
            root: self.dest.to_path_buf(),
            files: have,
            release,
        })
    }
}

// ---- tar.gz (USTAR, regular files only) ----

fn octal(field: &[u8]) -> Result<u64, Error> {
    require(
        field.first().is_none_or(|b| b & 0x80 == 0),
        ErrorCode::Archive,
        "tar base-256 numbers aren't accepted",
    )?;
    let start = field.iter().position(|&b| b != b' ').unwrap_or(field.len());
    let text: Vec<u8> = field[start..]
        .iter()
        .copied()
        .take_while(|&b| b != 0 && b != b' ')
        .collect();
    require(
        !text.is_empty() && text.iter().all(|c| (b'0'..=b'7').contains(c)),
        ErrorCode::Archive,
        "bad tar number",
    )?;
    let mut value: u64 = 0;
    for c in text {
        value = value
            .checked_mul(8)
            .and_then(|v| v.checked_add(u64::from(c - b'0')))
            .ok_or_else(|| archive_error("bad tar number"))?;
    }
    Ok(value)
}

fn cstr(field: &[u8]) -> Result<&str, Error> {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    std::str::from_utf8(&field[..end]).map_err(|_| archive_error("tar name isn't UTF-8"))
}

/// Extract a `.tar.gz` release archive whose top folder is `top`.
pub fn extract_tar_gz(
    reader: impl Read,
    top: &str,
    dest: &Path,
    expect: &Expect,
) -> Result<VerifiedTree, Error> {
    let mut sink = Sink::new(dest, top)?;
    let mut tar = flate2::read::GzDecoder::new(reader.take(MAX_ARCHIVE));
    let mut header = [0u8; 512];
    loop {
        tar.read_exact(&mut header)
            .map_err(|_| archive_error("tar archive is truncated or corrupt"))?;
        if header.iter().all(|&b| b == 0) {
            tar.read_exact(&mut header)
                .map_err(|_| archive_error("tar archive is truncated"))?;
            require(
                header.iter().all(|&b| b == 0),
                ErrorCode::Archive,
                "tar end marker is malformed",
            )?;
            let mut rest = [0u8; 4096];
            loop {
                let n = tar
                    .read(&mut rest)
                    .map_err(|_| archive_error("tar archive is corrupt"))?;
                if n == 0 {
                    break;
                }
                require(
                    rest[..n].iter().all(|&b| b == 0),
                    ErrorCode::Archive,
                    "data after the tar end marker",
                )?;
            }
            break;
        }
        let stored = octal(&header[148..156])?;
        let sum: u64 = header
            .iter()
            .enumerate()
            .map(|(i, &b)| {
                if (148..156).contains(&i) {
                    32
                } else {
                    u64::from(b)
                }
            })
            .sum();
        require(
            stored == sum,
            ErrorCode::Archive,
            "tar header checksum mismatch",
        )?;
        require(
            &header[257..263] == b"ustar\0" && &header[263..265] == b"00",
            ErrorCode::Archive,
            "only USTAR tar archives are accepted",
        )?;
        require(
            matches!(header[156], b'0' | 0),
            ErrorCode::Archive,
            "archive has a link, folder or special entry",
        )?;
        let name = cstr(&header[0..100])?;
        let prefix = cstr(&header[345..500])?;
        let full = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}/{name}")
        };
        let mode = octal(&header[100..108])?;
        let size = octal(&header[124..136])?;
        require(
            size <= MAX_ARCHIVE,
            ErrorCode::TooLarge,
            "archive entry is too large",
        )?;
        sink.file(&full, size, mode & 0o111 != 0, &mut tar)?;
        let pad = (512 - size % 512) % 512;
        let mut skip = [0u8; 512];
        tar.read_exact(&mut skip[..pad as usize])
            .map_err(|_| archive_error("tar archive is truncated"))?;
    }
    sink.finish(expect)
}

// ---- zip (stored or deflated regular files, no ZIP64, no encryption) ----

fn u16le(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}
fn u32le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

struct CentralEntry {
    name: String,
    method: u16,
    crc: u32,
    compressed: u64,
    size: u64,
    executable: bool,
    local_offset: u64,
}

/// Extract a `.zip` release archive whose top folder is `top`.
pub fn extract_zip(
    file: &mut File,
    top: &str,
    dest: &Path,
    expect: &Expect,
) -> Result<VerifiedTree, Error> {
    let corrupt = || archive_error("zip archive is malformed");
    let len = file
        .metadata()
        .map_err(|e| io_error("reading the archive", e))?
        .len();
    require(
        (22..=MAX_ARCHIVE).contains(&len),
        ErrorCode::Archive,
        "zip archive has a bad size",
    )?;
    let tail_len = len.min(22 + 65_535);
    let mut tail = vec![0u8; tail_len as usize];
    file.seek(SeekFrom::Start(len - tail_len))
        .map_err(|e| io_error("reading the archive", e))?;
    file.read_exact(&mut tail)
        .map_err(|e| io_error("reading the archive", e))?;
    let eocd = (0..=tail.len() - 22)
        .rev()
        .find(|&i| {
            u32le(&tail, i) == 0x0605_4b50
                && i + 22 + usize::from(u16le(&tail, i + 20)) == tail.len()
        })
        .ok_or_else(corrupt)?;
    let e = &tail[eocd..];
    require(
        u16le(e, 4) == 0 && u16le(e, 6) == 0,
        ErrorCode::Archive,
        "multi-disk zip archives aren't accepted",
    )?;
    let count = u16le(e, 10);
    require(
        u16le(e, 8) == count && count != 0xffff,
        ErrorCode::Archive,
        "zip64 archives aren't accepted",
    )?;
    let cd_size = u64::from(u32le(e, 12));
    let cd_offset = u64::from(u32le(e, 16));
    require(
        cd_size != 0xffff_ffff
            && cd_offset != 0xffff_ffff
            && cd_offset + cd_size <= len - tail_len + eocd as u64,
        ErrorCode::Archive,
        "zip central directory is malformed",
    )?;
    require(
        usize::from(count) <= MAX_ENTRIES,
        ErrorCode::TooLarge,
        "archive has too many entries",
    )?;
    let mut cd = vec![0u8; cd_size as usize];
    file.seek(SeekFrom::Start(cd_offset))
        .map_err(|e| io_error("reading the archive", e))?;
    file.read_exact(&mut cd)
        .map_err(|e| io_error("reading the archive", e))?;

    let mut entries = Vec::with_capacity(usize::from(count));
    let mut at = 0usize;
    for _ in 0..count {
        require(
            at + 46 <= cd.len() && u32le(&cd, at) == 0x0201_4b50,
            ErrorCode::Archive,
            "zip central directory is malformed",
        )?;
        let made_by_os = cd[at + 5];
        let flags = u16le(&cd, at + 8);
        let method = u16le(&cd, at + 10);
        let crc = u32le(&cd, at + 16);
        let compressed = u64::from(u32le(&cd, at + 20));
        let size = u64::from(u32le(&cd, at + 24));
        let name_len = usize::from(u16le(&cd, at + 28));
        let extra_len = usize::from(u16le(&cd, at + 30));
        let comment_len = usize::from(u16le(&cd, at + 32));
        let disk = u16le(&cd, at + 34);
        let external = u32le(&cd, at + 38);
        let local_offset = u64::from(u32le(&cd, at + 42));
        let end = at + 46 + name_len + extra_len + comment_len;
        require(
            end <= cd.len(),
            ErrorCode::Archive,
            "zip central directory is malformed",
        )?;
        let name = std::str::from_utf8(&cd[at + 46..at + 46 + name_len])
            .map_err(|_| corrupt())?
            .to_string();
        require(
            flags & 0x1 == 0 && flags & 0x40 == 0,
            ErrorCode::Archive,
            "encrypted zip entries aren't accepted",
        )?;
        require(
            matches!(method, 0 | 8),
            ErrorCode::Archive,
            "zip compression method isn't accepted",
        )?;
        require(
            disk == 0,
            ErrorCode::Archive,
            "multi-disk zip archives aren't accepted",
        )?;
        require(
            compressed != 0xffff_ffff && size != 0xffff_ffff && local_offset != 0xffff_ffff,
            ErrorCode::Archive,
            "zip64 archives aren't accepted",
        )?;
        // Unix-made entries carry a mode: regular files only.
        let unix_mode = if made_by_os == 3 { external >> 16 } else { 0 };
        if made_by_os == 3 {
            require(
                unix_mode & 0o170000 == 0o100000,
                ErrorCode::Archive,
                "archive has a link, folder or special entry",
            )?;
        }
        require(
            !name.ends_with('/') && external & 0x10 == 0,
            ErrorCode::Archive,
            "archive has a folder entry",
        )?;
        entries.push(CentralEntry {
            name,
            method,
            crc,
            compressed,
            size,
            executable: unix_mode & 0o111 != 0,
            local_offset,
        });
        at = end;
    }
    require(
        at == cd.len(),
        ErrorCode::Archive,
        "zip central directory is malformed",
    )?;

    let mut sink = Sink::new(dest, top)?;
    for entry in entries {
        file.seek(SeekFrom::Start(entry.local_offset))
            .map_err(|e| io_error("reading the archive", e))?;
        let mut local = [0u8; 30];
        file.read_exact(&mut local).map_err(|_| corrupt())?;
        require(
            u32le(&local, 0) == 0x0403_4b50 && u16le(&local, 8) == entry.method,
            ErrorCode::Archive,
            "zip local header is malformed",
        )?;
        let name_len = u64::from(u16le(&local, 26));
        let extra_len = u64::from(u16le(&local, 28));
        let mut name = vec![0u8; name_len as usize];
        file.read_exact(&mut name).map_err(|_| corrupt())?;
        require(
            name == entry.name.as_bytes(),
            ErrorCode::Archive,
            "zip local and central names differ",
        )?;
        let data_start = entry.local_offset + 30 + name_len + extra_len;
        require(
            data_start + entry.compressed <= cd_offset,
            ErrorCode::Archive,
            "zip entry overlaps the directory",
        )?;
        file.seek(SeekFrom::Start(data_start))
            .map_err(|e| io_error("reading the archive", e))?;
        let raw = (&mut *file).take(entry.compressed);
        let reader: Box<dyn Read + '_> = if entry.method == 8 {
            Box::new(flate2::read::DeflateDecoder::new(raw))
        } else {
            Box::new(raw)
        };
        let mut checked = CrcReader {
            inner: reader,
            crc: flate2::Crc::new(),
        };
        sink.file(&entry.name, entry.size, entry.executable, &mut checked)?;
        let mut extra = [0u8; 1];
        require(
            matches!(checked.inner.read(&mut extra), Ok(0)),
            ErrorCode::Archive,
            "zip entry is longer than its directory says",
        )?;
        require(
            checked.crc.sum() == entry.crc,
            ErrorCode::Archive,
            "zip entry CRC mismatch",
        )?;
    }
    sink.finish(expect)
}

struct CrcReader<R: Read> {
    inner: R,
    crc: flate2::Crc,
}

impl<R: Read> Read for CrcReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.crc.update(&buf[..n]);
        Ok(n)
    }
}

/// Stream a file's SHA-256 (bounded by `MAX_ARCHIVE`).
pub fn sha256_file(path: &Path) -> Result<[u8; 32], Error> {
    let mut file = File::open(path).map_err(|e| io_error("reading a download", e))?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| io_error("reading a download", e))?;
        if n == 0 {
            break;
        }
        total += n as u64;
        require(
            total <= MAX_ARCHIVE,
            ErrorCode::TooLarge,
            "download is too large",
        )?;
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().into())
}

/// For logs: a digest's short form.
pub fn short(digest: &[u8; 32]) -> String {
    hex(digest)[..12].to_string()
}
