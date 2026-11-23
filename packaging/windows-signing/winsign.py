#!/usr/bin/env python3
"""Verify, stage and finalize Metro's Windows distribution around Authenticode.

This helper never signs anything and never holds a key. It:

* verifies the trusted upstream seed (``seed-desktop-windows-x64``) before
  anything is signed: exact archive layout, RELEASE.json, the pinned stock NSH
  renderer, the binary allowlist and the NSH/icon sources it was built from;
* prepares a signing directory that holds only the allowlisted files;
* after signing, proves that each signed file differs from its unsigned seed
  only in signature-related bytes, inspects the Authenticode PKCS#7 structure
  (SHA-256, one signature, code-signing EKU, publisher, RFC 3161 SHA-256
  timestamp bound to the signature), and repacks the portable ZIP;
* verifies the final distribution directory (exact file set, checksums,
  re-extracted ZIP, MSI and script signatures) and writes the aggregate
  checksum list.

Operating-system trust (``signtool verify /pa /all /tw``) is checked separately
on a fresh Windows VM by MetroWindowsSigning.psm1. Standard library only; run
with ``python -I``.
"""

import argparse
import base64
import binascii
import datetime as dt
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import struct
import sys
import tempfile
import tomllib
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import package  # noqa: E402  (packaging/package.py: pinned NSH and icon checks)

TARGET = "windows-x64"
# Fixed allowlist of everything that gets an Authenticode signature. Official
# releases need all four; metro-tui.exe is optional only in preview mode, for
# seeds that predate the TUI.
REQUIRED_EXES = ("Metro.exe", "metro-server.exe", "ui/Metro.exe")
TUI_EXE = "metro-tui.exe"
SIGNED_EXES = (*REQUIRED_EXES, TUI_EXE)
SCRIPTS = ("install-metro-windows.ps1", "metro-runner-container.ps1")
STOCK_RENDERER_SHA256 = package.NSH_APPS["windows"][1]
ICON_FRAMES = 7
NSH_DIR_ENTRIES = {"app.json", "bundle.nshc", "capabilities.json", "assets", "icon.ico"}
MANIFEST_KEYS = {"connector", "dependencies", "files", "kind", "local_runner", "nsh", "target", "version"}
# Anything that Windows could execute or load, judged by name. Every member is
# also checked for an MZ header, so a renamed PE is caught too.
EXECUTABLE_SUFFIXES = (".exe", ".dll", ".sys", ".com", ".scr", ".cpl", ".ocx", ".msi", ".msp", ".msix",
                       ".appx", ".ps1", ".psm1", ".psd1", ".bat", ".cmd", ".vbs", ".vbe", ".js", ".jse",
                       ".wsf", ".wsh", ".hta", ".lnk", ".reg", ".jar")
SEMVER = re.compile(r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]{1,40})?")
HEX64 = re.compile(r"[0-9a-f]{64}")
# The only context in which official (signing) mode may run: a push, not a
# deletion, to the protected literal master branch of this repository, whose
# top-level workflow is ci.yml on master, building exactly the pushed commit.
# Tags, pull requests and any other branch are never official.
OFFICIAL_REPOSITORY = "wnzn/metro"
OFFICIAL_REPOSITORY_ID = "1467654868"
OFFICIAL_REF = "refs/heads/master"
OFFICIAL_WORKFLOW_REF = f"{OFFICIAL_REPOSITORY}/.github/workflows/ci.yml@{OFFICIAL_REF}"
SHA = re.compile(r"[0-9a-f]{40}")

# The MSI's local-runner choice installs this empty file next to Metro.exe
# (packaging/windows-msi); it is the only file an installed folder may add.
MARKER = "enable-local-runner"
# Largest spread between the earliest and latest timestamp in one release.
MAX_TIMESTAMP_SPREAD = dt.timedelta(hours=12)

# OIDs
OID_SIGNED_DATA = "1.2.840.113549.1.7.2"
OID_SPC_INDIRECT = "1.3.6.1.4.1.311.2.1.4"
OID_SHA256 = "2.16.840.1.101.3.4.2.1"
OID_RFC3161 = "1.3.6.1.4.1.311.3.3.1"
OID_NESTED = "1.3.6.1.4.1.311.2.4.1"
OID_TSTINFO = "1.2.840.113549.1.9.16.1.4"
OID_EKU = "2.5.29.37"
OID_CODE_SIGNING = "1.3.6.1.5.5.7.3.3"
OID_TIME_STAMPING = "1.3.6.1.5.5.7.3.8"
OID_CN = "2.5.4.3"
OID_O = "2.5.4.10"
SIGNATURE_ALGORITHMS = {
    "1.2.840.113549.1.1.1",   # rsaEncryption
    "1.2.840.113549.1.1.10",  # RSASSA-PSS
    "1.2.840.113549.1.1.11",  # sha256WithRSAEncryption
    "1.2.840.113549.1.1.12",  # sha384WithRSAEncryption
    "1.2.840.113549.1.1.13",  # sha512WithRSAEncryption
    "1.2.840.10045.4.3.2",    # ecdsa-with-SHA256
    "1.2.840.10045.4.3.3",    # ecdsa-with-SHA384
    "1.2.840.10045.2.1",      # id-ecPublicKey (used by some CMS signers)
}


class Failure(ValueError):
    pass


def require(condition, message):
    if not condition:
        raise Failure(message)


def sha256_bytes(data):
    return hashlib.sha256(data).hexdigest()


def sha256_file(path):
    with open(path, "rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def regular(path):
    path = Path(path)
    require(not path.is_symlink() and path.is_file(), f"not a regular file: {path}")
    return path


def safe_relative(rel):
    parts = PurePosixPath(rel).parts
    require(rel and "\\" not in rel and ":" not in rel and not rel.startswith("/")
            and all(p not in ("", ".", "..") for p in parts) and "/".join(parts) == rel,
            f"unsafe path: {rel!r}")
    return rel


def tree_files(root):
    """Relative POSIX paths of every file below root; links and junctions are refused."""
    root = Path(root)
    found = {}
    for here, dirs, names in os.walk(root):
        for name in dirs:
            d = Path(here) / name
            junction = getattr(d, "is_junction", lambda: False)
            require(not d.is_symlink() and not junction(), f"link in tree: {d}")
        for name in names:
            p = Path(here) / name
            require(not p.is_symlink() and p.is_file(), f"not a regular file in tree: {p}")
            found[p.relative_to(root).as_posix()] = p
    return found


def read_sidecar(path, name):
    text = regular(path).read_text(encoding="ascii")
    match = re.fullmatch(r"([0-9a-f]{64})  (\S+)\n", text)
    require(match and match.group(2) == name, f"{path}: expected exactly '<sha256>  {name}'")
    return match.group(1)


def write_sidecar(path, name):
    digest = sha256_file(path)
    Path(f"{path}.sha256").write_bytes(f"{digest}  {name}\n".encode("ascii"))
    return digest


# --------------------------------------------------------------------------- DER

class Node:
    __slots__ = ("tag", "value", "raw")

    def __init__(self, tag, value, raw):
        self.tag, self.value, self.raw = tag, value, raw

    @property
    def constructed(self):
        return bool(self.tag & 0x20)

    def children(self):
        require(self.constructed, f"DER tag {self.tag:#x} is not constructed")
        return parse_all(self.value)


def parse_one(buf, pos=0):
    require(pos + 2 <= len(buf), "truncated DER")
    tag = buf[pos]
    require(tag & 0x1F != 0x1F, "multi-byte DER tags are not supported")
    first = buf[pos + 1]
    pos2 = pos + 2
    if first < 0x80:
        length = first
    else:
        n = first & 0x7F
        require(1 <= n <= 4 and pos2 + n <= len(buf), "indefinite or oversized DER length")
        length = int.from_bytes(buf[pos2:pos2 + n], "big")
        require(length >= 0x80 and (n == 1 or buf[pos2] != 0), "non-minimal DER length")
        pos2 += n
    end = pos2 + length
    require(end <= len(buf), "DER value runs past its container")
    return Node(tag, bytes(buf[pos2:end]), bytes(buf[pos:end])), end


def parse_all(buf):
    nodes, pos = [], 0
    while pos < len(buf):
        node, pos = parse_one(buf, pos)
        nodes.append(node)
    return nodes


def parse_exact(buf):
    node, end = parse_one(buf, 0)
    require(end == len(buf), "trailing bytes after DER object")
    return node


def expect(node, tag, what):
    require(node.tag == tag, f"{what}: expected DER tag {tag:#x}, found {node.tag:#x}")
    return node


def oid(node):
    expect(node, 0x06, "OID")
    data = node.value
    require(data, "empty OID")
    arcs, value = [], 0
    for b in data:
        value = (value << 7) | (b & 0x7F)
        if not b & 0x80:
            arcs.append(value)
            value = 0
    require(not data[-1] & 0x80, "truncated OID")
    first = min(arcs[0] // 40, 2)
    return ".".join(map(str, [first, arcs[0] - 40 * first, *arcs[1:]]))


def algorithm(node):
    return oid(expect(node, 0x30, "AlgorithmIdentifier").children()[0])


def decode_string(node):
    if node.tag == 0x1E:
        return node.value.decode("utf-16-be")
    if node.tag in (0x0C, 0x13, 0x16, 0x1A):
        return node.value.decode("utf-8")
    if node.tag == 0x14:
        return node.value.decode("latin-1")
    raise Failure(f"unsupported directory string tag {node.tag:#x}")


def name_attributes(name):
    attrs = {}
    for rdn in expect(name, 0x30, "Name").children():
        for atv in expect(rdn, 0x31, "RDN").children():
            kind, value = atv.children()
            attrs.setdefault(oid(kind), []).append(decode_string(value))
    return attrs


def parse_time(node):
    text = node.value.decode("ascii")
    if node.tag == 0x17:
        require(re.fullmatch(r"\d{12}Z", text), "invalid UTCTime")
        year = int(text[:2])
        text = ("19" if year >= 50 else "20") + text
    else:
        expect(node, 0x18, "GeneralizedTime")
        text = re.sub(r"\.\d+Z$", "Z", text)
        require(re.fullmatch(r"\d{14}Z", text), "invalid GeneralizedTime")
    return dt.datetime.strptime(text, "%Y%m%d%H%M%SZ").replace(tzinfo=dt.timezone.utc)


class Certificate:
    def __init__(self, node):
        tbs = expect(node, 0x30, "Certificate").children()[0]
        items = expect(tbs, 0x30, "TBSCertificate").children()
        if items[0].tag == 0xA0:
            items = items[1:]
        self.raw = node.raw
        self.serial = expect(items[0], 0x02, "serial").value
        self.issuer_raw = items[2].raw
        self.subject_raw = items[4].raw
        self.issuer = name_attributes(items[2])
        self.subject = name_attributes(items[4])
        validity = expect(items[3], 0x30, "Validity").children()
        self.not_before, self.not_after = parse_time(validity[0]), parse_time(validity[1])
        self.eku = None
        self.ski = None
        for extra in items[6:]:
            if extra.tag != 0xA3:
                continue
            for ext in expect(extra.children()[0], 0x30, "Extensions").children():
                parts = ext.children()
                ext_oid, value = oid(parts[0]), expect(parts[-1], 0x04, "extnValue").value
                if ext_oid == OID_EKU:
                    self.eku = {oid(n) for n in parse_exact(value).children()}
                elif ext_oid == "2.5.29.14":
                    self.ski = expect(parse_exact(value), 0x04, "SKI").value

    def matches(self, sid):
        if sid.tag == 0x30:
            issuer, serial = sid.children()
            return issuer.raw == self.issuer_raw and expect(serial, 0x02, "serial").value == self.serial
        if sid.tag == 0x80:
            return self.ski is not None and sid.value == self.ski
        return False

    def first(self, which, attr):
        values = getattr(self, which).get(attr, [])
        return values[0] if len(values) == 1 else None


class SignedData:
    def __init__(self, blob, what):
        info = expect(parse_exact(blob), 0x30, f"{what}: ContentInfo").children()
        require(len(info) == 2 and oid(info[0]) == OID_SIGNED_DATA, f"{what}: not PKCS#7 SignedData")
        sd = expect(expect(info[1], 0xA0, f"{what}: content").children()[0], 0x30, f"{what}: SignedData").children()
        self.what = what
        self.digest_algorithms = [algorithm(n) for n in expect(sd[1], 0x31, "digestAlgorithms").children()]
        encap = expect(sd[2], 0x30, "encapContentInfo").children()
        self.content_type = oid(encap[0])
        self.content = None
        if len(encap) > 1:
            inner = expect(encap[1], 0xA0, "eContent").children()[0]
            self.content = parse_exact(inner.value) if inner.tag == 0x04 else inner
        self.certificates = []
        rest = sd[3:]
        while rest and rest[0].tag in (0xA0, 0xA1):
            if rest[0].tag == 0xA0:
                self.certificates = [Certificate(n) for n in rest[0].children() if n.tag == 0x30]
            rest = rest[1:]
        require(len(rest) == 1, f"{what}: unexpected SignedData layout")
        self.signers = expect(rest[0], 0x31, "signerInfos").children()

    def single_signer(self):
        require(len(self.signers) == 1, f"{self.what}: expected exactly one SignerInfo, found {len(self.signers)}")
        items = expect(self.signers[0], 0x30, "SignerInfo").children()
        signer = {"sid": items[1], "digest": algorithm(items[2]), "signed": None, "unsigned": None}
        rest = items[3:]
        if rest and rest[0].tag == 0xA0:
            signer["signed"] = rest[0]
            rest = rest[1:]
        signer["signature_algorithm"] = algorithm(rest[0])
        signer["signature"] = expect(rest[1], 0x04, "signature").value
        if len(rest) > 2:
            signer["unsigned"] = expect(rest[2], 0xA1, "unsignedAttrs")
            require(len(rest) == 3, f"{self.what}: unexpected SignerInfo layout")
        certs = [c for c in self.certificates if c.matches(signer["sid"])]
        require(len(certs) == 1, f"{self.what}: the signer certificate is not in the signature")
        signer["certificate"] = certs[0]
        return signer


def attributes(node):
    found = {}
    if node is None:
        return found
    for attr in node.children():
        kind, values = attr.children()
        found.setdefault(oid(kind), []).append(expect(values, 0x31, "attribute values").children())
    return found


def inspect_signature(blob, what, publisher_cn=None, publisher_org=None):
    """Structural Authenticode checks. Cryptographic trust is checked by Windows."""
    sd = SignedData(blob, what)
    require(sd.digest_algorithms == [OID_SHA256], f"{what}: digest algorithms must be exactly SHA-256")
    require(sd.content_type == OID_SPC_INDIRECT and sd.content is not None,
            f"{what}: not an Authenticode (SPC_INDIRECT_DATA) signature")
    indirect = expect(sd.content, 0x30, "SpcIndirectDataContent").children()
    digest_info = expect(indirect[1], 0x30, "DigestInfo").children()
    require(algorithm(digest_info[0]) == OID_SHA256 and len(expect(digest_info[1], 0x04, "digest").value) == 32,
            f"{what}: the Authenticode file digest is not SHA-256")
    signer = sd.single_signer()
    require(signer["digest"] == OID_SHA256, f"{what}: the signer digest is not SHA-256")
    require(signer["signature_algorithm"] in SIGNATURE_ALGORITHMS,
            f"{what}: unexpected signature algorithm {signer['signature_algorithm']}")
    cert = signer["certificate"]
    require(cert.eku is not None and OID_CODE_SIGNING in cert.eku, f"{what}: signer certificate lacks the code-signing EKU")
    require(cert.issuer_raw != cert.subject_raw, f"{what}: the signer certificate is self-signed")
    cn, org = cert.first("subject", OID_CN), cert.first("subject", OID_O)
    if publisher_cn is not None:
        require(publisher_cn and cn == publisher_cn, f"{what}: publisher CN {cn!r} is not the expected {publisher_cn!r}")
    if publisher_org:
        require(org == publisher_org, f"{what}: publisher O {org!r} is not the expected {publisher_org!r}")

    unsigned = attributes(signer["unsigned"])
    require(OID_NESTED not in unsigned, f"{what}: nested (additional) signatures are not allowed")
    require(set(unsigned) == {OID_RFC3161} and len(unsigned[OID_RFC3161]) == 1
            and len(unsigned[OID_RFC3161][0]) == 1,
            f"{what}: expected exactly one RFC 3161 timestamp and no other unsigned attributes")
    token = SignedData(unsigned[OID_RFC3161][0][0].raw, f"{what} timestamp")
    require(token.content_type == OID_TSTINFO and token.content is not None, f"{what}: timestamp is not RFC 3161 TSTInfo")
    require(token.digest_algorithms == [OID_SHA256], f"{what}: the timestamp digest is not SHA-256")
    tst = expect(token.content, 0x30, "TSTInfo").children()
    imprint = expect(tst[2], 0x30, "MessageImprint").children()
    require(algorithm(imprint[0]) == OID_SHA256, f"{what}: the timestamp message imprint is not SHA-256")
    require(expect(imprint[1], 0x04, "imprint").value == hashlib.sha256(signer["signature"]).digest(),
            f"{what}: the timestamp is not bound to this signature")
    when = parse_time(tst[4])
    tsa = token.single_signer()
    require(tsa["digest"] == OID_SHA256, f"{what}: the timestamp signer digest is not SHA-256")
    require(tsa["certificate"].eku is not None and OID_TIME_STAMPING in tsa["certificate"].eku,
            f"{what}: the timestamp signer lacks the time-stamping EKU")
    require(cert.not_before <= when <= cert.not_after,
            f"{what}: the timestamp is outside the signer certificate's validity")
    return {"publisher_cn": cn, "publisher_org": org, "timestamp": when.isoformat(),
            "signer_sha256": sha256_bytes(cert.raw)}


# --------------------------------------------------------------------------- PE

def pe_layout(data, what):
    require(data[:2] == b"MZ" and len(data) >= 0x40, f"{what}: not a PE file")
    header = struct.unpack_from("<I", data, 0x3C)[0]
    opt = header + 24
    require(data[header:header + 4] == b"PE\0\0" and struct.unpack_from("<H", data, opt)[0] == 0x20B,
            f"{what}: not a PE32+ image")
    require(struct.unpack_from("<I", data, opt + 108)[0] > 4, f"{what}: no certificate table directory")
    return opt + 64, opt + 112 + 8 * 4  # CheckSum, IMAGE_DIRECTORY_ENTRY_SECURITY


def pe_unsigned_info(data, what):
    checksum_at, security_at = pe_layout(data, what)
    require(data[security_at:security_at + 8] == b"\0" * 8, f"{what}: the seed is already signed")
    return {"unsigned_sha256": sha256_bytes(data), "unsigned_size": len(data),
            "unsigned_pe_checksum": struct.unpack_from("<I", data, checksum_at)[0]}


def pe_split(data, info, what):
    """Return (unsigned bytes, PKCS#7) or fail if anything but signature bytes changed."""
    checksum_at, security_at = pe_layout(data, what)
    offset, size = struct.unpack_from("<II", data, security_at)
    original = info["unsigned_size"]
    require(offset and size, f"{what}: not signed (no certificate table)")
    require(offset % 8 == 0 and original <= offset < original + 8 and offset + size == len(data),
            f"{what}: the certificate table is not a single trailing block after the original image")
    require(data[original:offset] == b"\0" * (offset - original), f"{what}: non-zero bytes before the certificate table")
    length, revision, kind = struct.unpack_from("<IHH", data, offset)
    require(revision == 0x0200 and kind == 0x0002 and 8 < length <= size and size == (length + 7) // 8 * 8
            and data[offset + length:] == b"\0" * (size - length),
            f"{what}: expected exactly one WIN_CERT_TYPE_PKCS_SIGNED_DATA entry")
    blob = data[offset + 8:offset + length]
    # DER may be followed by zero padding inside dwLength.
    node, end = parse_one(blob, 0)
    require(blob[end:] == b"\0" * (len(blob) - end), f"{what}: data after the PKCS#7 signature")
    restored = bytearray(data[:original])
    struct.pack_into("<I", restored, checksum_at, info["unsigned_pe_checksum"])
    struct.pack_into("<II", restored, security_at, 0, 0)
    require(sha256_bytes(restored) == info["unsigned_sha256"],
            f"{what}: bytes other than the checksum and signature changed during signing")
    return bytes(restored), node.raw


# --------------------------------------------------------------------------- PowerShell

SIG_BLOCK = re.compile(rb"\A(?:\r?\n)?# SIG # Begin signature block\r?\n((?:# [A-Za-z0-9+/=]{1,128}\r?\n)+)"
                       rb"# SIG # End signature block\r?\n?\Z")


def script_unsigned_info(data, what):
    require(b"# SIG # Begin signature block" not in data, f"{what}: the source script already has a signature block")
    require(max(data, default=0) < 0x80 and not data.startswith(b"\xef\xbb\xbf"),
            f"{what}: expected a plain ASCII script")
    return {"unsigned_sha256": sha256_bytes(data), "unsigned_size": len(data)}


def script_split(data, info, what):
    original = data[:info["unsigned_size"]]
    require(sha256_bytes(original) == info["unsigned_sha256"], f"{what}: script content changed during signing")
    match = SIG_BLOCK.match(data[info["unsigned_size"]:])
    require(match, f"{what}: expected exactly one PowerShell signature block appended to the unchanged script")
    lines = re.findall(rb"# ([A-Za-z0-9+/=]+)\r?\n", match.group(1))
    try:
        blob = base64.b64decode(b"".join(lines), validate=True)
    except binascii.Error as error:
        raise Failure(f"{what}: invalid signature block encoding") from error
    return original, parse_exact(blob).raw


# --------------------------------------------------------------------------- MSI (CFB)

def cfb_stream(data, wanted, what):
    """Return the named root-level stream of an OLE compound file, or None."""
    require(data[:8] == bytes.fromhex("d0cf11e0a1b11ae1") and len(data) >= 512, f"{what}: not an MSI (OLE) file")
    shift, mini_shift = struct.unpack_from("<HH", data, 30)
    require(shift in (9, 12) and mini_shift == 6, f"{what}: unsupported compound file sector sizes")
    size = 1 << shift
    fat_count, dir_start = struct.unpack_from("<II", data, 44)
    cutoff, mini_fat_start, mini_fat_count, difat_start, difat_count = struct.unpack_from("<IIIII", data, 56)
    require(cutoff == 4096, f"{what}: unexpected mini stream cutoff")

    def sector(n):
        start = (n + 1) * size
        require(n < 0xFFFFFFFA and start + size <= len(data), f"{what}: compound file sector out of range")
        return data[start:start + size]

    difat = list(struct.unpack_from("<109I", data, 76))
    seen = set()
    while difat_count and difat_start not in (0xFFFFFFFE, 0xFFFFFFFF):
        require(difat_start not in seen, f"{what}: DIFAT loop")
        seen.add(difat_start)
        entries = struct.unpack(f"<{size // 4}I", sector(difat_start))
        difat.extend(entries[:-1])
        difat_start = entries[-1]
        difat_count -= 1
    fat = []
    for n in difat[:fat_count]:
        fat.extend(struct.unpack(f"<{size // 4}I", sector(n)))

    def chain(start, table, limit):
        out, n = [], start
        while n != 0xFFFFFFFE:
            require(n < len(table) and len(out) <= limit, f"{what}: broken compound file chain")
            out.append(n)
            n = table[n]
        return out

    directory = b"".join(sector(n) for n in chain(dir_start, fat, len(fat)))
    entries = [directory[i:i + 128] for i in range(0, len(directory), 128)]

    def entry(i):
        e = entries[i]
        name_len = struct.unpack_from("<H", e, 64)[0]
        require(name_len % 2 == 0 and name_len <= 64, f"{what}: invalid directory entry")
        name = e[:max(name_len - 2, 0)].decode("utf-16-le")
        kind = e[66]
        left, right, child, = struct.unpack_from("<III", e, 68)
        start, length = struct.unpack_from("<IQ", e, 116)
        return name, kind, left, right, child, start, length

    root = entry(0)
    require(root[1] == 5, f"{what}: missing compound file root")
    mini_stream = b"".join(sector(n) for n in chain(root[5], fat, len(fat)))[:root[6]] if root[5] != 0xFFFFFFFE else b""
    mini_fat = []
    if mini_fat_count:
        for n in chain(mini_fat_start, fat, len(fat)):
            mini_fat.extend(struct.unpack(f"<{size // 4}I", sector(n)))

    # Walk the red-black tree of the root storage's children.
    found, stack, visited = [], [root[4]], set()
    while stack:
        i = stack.pop()
        if i == 0xFFFFFFFF:
            continue
        require(i < len(entries) and i not in visited, f"{what}: invalid directory tree")
        visited.add(i)
        name, kind, left, right, _child, start, length = entry(i)
        stack += [left, right]
        if name == wanted:
            require(kind == 2, f"{what}: {wanted!r} is not a stream")
            found.append((start, length))
    if not found:
        return None
    require(len(found) == 1, f"{what}: duplicate {wanted!r} streams")
    start, length = found[0]
    if length < cutoff:
        chunks = [mini_stream[n * 64:(n + 1) * 64] for n in chain(start, mini_fat, len(mini_fat))]
    else:
        chunks = [sector(n) for n in chain(start, fat, len(fat))]
    stream = b"".join(chunks)
    require(len(stream) >= length, f"{what}: truncated stream")
    return stream[:length]


def msi_signature(path):
    data = regular(path).read_bytes()
    blob = cfb_stream(data, "\x05DigitalSignature", path.name)
    if blob is None:
        return None
    node, end = parse_one(blob, 0)
    require(blob[end:] == b"\0" * (len(blob) - end), f"{path.name}: data after the PKCS#7 signature")
    return node.raw


# --------------------------------------------------------------------------- seed

def zip_members(archive, root):
    infos = archive.infolist()
    names = [i.filename for i in infos]
    require(len(names) == len(set(names)), "duplicate ZIP entries")
    members = {}
    for info in infos:
        require(info.filename.startswith(root + "/"), f"ZIP entry outside {root}/: {info.filename!r}")
        rel = safe_relative(info.filename[len(root) + 1:])
        mode = info.external_attr >> 16
        require(info.create_system == 3 and stat.S_ISREG(mode) and not info.flag_bits & 0x1
                and info.compress_type in (zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED),
                f"unexpected ZIP entry type, flags or compression: {info.filename}")
        members[rel] = info
    return members


def check_modes(members):
    for rel, info in members.items():
        require(stat.S_IMODE(info.external_attr >> 16) == (0o755 if rel.endswith(".exe") else 0o644),
                f"unexpected file mode in the ZIP: {rel}")


def exes_in(files, official):
    """The signable EXEs a manifest lists: all four officially, the TUI optional in preview."""
    present = tuple(rel for rel in SIGNED_EXES if rel in files)
    require(present == SIGNED_EXES if official else set(REQUIRED_EXES) <= set(present),
            f"expected EXEs {list(SIGNED_EXES if official else REQUIRED_EXES)}, found {list(present)}")
    return present


def check_manifest(manifest, version, signing, official):
    keys = MANIFEST_KEYS | ({"signing"} if signing else set())
    require(set(manifest) == keys, f"RELEASE.json keys {sorted(manifest)} are not the expected {sorted(keys)}")
    require((manifest["version"], manifest["target"], manifest["kind"], manifest["nsh"], manifest["connector"],
             manifest["local_runner"]) == (version, TARGET, "desktop", package.NSH_VERSION, "linked", package.LOCAL_RUNNER),
            "RELEASE.json does not describe this Metro Windows desktop build")
    files = manifest["files"]
    require(isinstance(files, dict) and files and all(isinstance(v, str) and HEX64.fullmatch(v) for v in files.values()),
            "RELEASE.json file list is invalid")
    for rel in files:
        safe_relative(rel)
    require("RELEASE.json" not in files, "RELEASE.json must not list itself")
    exes = exes_in(files, official)
    require(set(manifest["dependencies"]) == set(exes), "RELEASE.json dependencies must cover exactly the shipped EXEs")
    return exes


def check_binaries(root, rels, exes):
    executable = sorted(r for r in rels if r.lower().endswith(EXECUTABLE_SUFFIXES)
                        or (root / r).read_bytes()[:2] == b"MZ")
    require(executable == sorted(exes), f"binary allowlist mismatch: {executable}")


def subsystem(path):
    data = regular(path).read_bytes()[:4096]
    header = struct.unpack_from("<I", data, 0x3C)[0]
    return struct.unpack_from("<H", data, header + 24 + 68)[0]


def check_sources(root, manifest, exes):
    """The seed matches the sources and pins this checkout builds from."""
    ico = package.app_icon("metro.ico")
    require(len(package.ico_images(ico)) == ICON_FRAMES, f"Metro's ICO must have {ICON_FRAMES} frames")
    package.verify_desktop_icons(root, True)
    deps = {"Metro.exe": package.pe(root / "Metro.exe", gui=True, icon=ico),
            "ui/Metro.exe": package.pe(root / "ui/Metro.exe", gui=True),
            "metro-server.exe": package.pe(root / "metro-server.exe")}
    if TUI_EXE in exes:
        deps[TUI_EXE] = package.pe(root / TUI_EXE)
        require(subsystem(root / TUI_EXE) == 3, "metro-tui.exe must be a console application")
        require(len({sha256_file(root / rel) for rel in exes}) == len(exes), "the EXEs must all be distinct")
    require(deps == manifest["dependencies"], "RELEASE.json dependencies differ from the EXEs' import tables")
    require(sha256_file(root / "ui/Metro.exe") == STOCK_RENDERER_SHA256,
            f"ui/Metro.exe is not the pinned stock NSH {package.NSH_VERSION} renderer")
    nsh = root / "ui/nsh"
    require({p.name for p in nsh.iterdir()} == NSH_DIR_ENTRIES, "unexpected entries in ui/nsh")
    app = package.read_json(nsh / "app.json")
    require((app.get("name"), app.get("id"), app.get("version")) == ("Metro", package.APP_ID, manifest["version"]),
            "ui/nsh/app.json does not describe this Metro build")
    require(not any(app.get(k) for k in ("singleInstance", "schemes", "fileTypes")),
            "the stock renderer must not register itself as the top-level launcher")
    declared = package.read_json(package.RENDERER_SOURCE / "nsh.json")["capabilities"]
    expected = {**declared, "network": {"http": [], **declared["network"]},
                "process": {"execute": [], "exit": False, **declared["process"]}}
    require(json.dumps(package.read_json(nsh / "capabilities.json"), sort_keys=True) == json.dumps(expected, sort_keys=True),
            "ui/nsh/capabilities.json differs from apps/desktop/nsh.json")
    package.nsh_assets(nsh / "assets", package.RENDERER_SOURCE)
    require(sha256_file(root / "licenses/NSH-LICENSE-APACHE.txt") == package.NSH_LICENSE_SHA, "unexpected NSH license")
    brand = package.RENDERER_SOURCE / "src/ui/icons/LICENSE-brand-marks.txt"
    require(sha256_file(root / "licenses/BRAND-MARKS.txt") == sha256_file(brand), "brand notice differs from source")


def extract(archive, members, root, dest):
    for rel, info in members.items():
        out = dest / rel
        out.parent.mkdir(parents=True, exist_ok=True)
        with archive.open(info) as src, open(out, "xb") as dst:
            shutil.copyfileobj(src, dst)


def verify_seed(args):
    version = args.version
    require(SEMVER.fullmatch(version), "version must be MAJOR.MINOR.PATCH[-pre]")
    name = f"Metro-{version}-{TARGET}.zip"
    seed = Path(args.seed_dir)
    require({p.name for p in seed.iterdir()} == {name, name + ".sha256"},
            f"the seed artifact must hold exactly {name} and its .sha256")
    zip_path = regular(seed / name)
    digest = read_sidecar(seed / (name + ".sha256"), name)
    require(sha256_file(zip_path) == digest, "seed ZIP does not match its .sha256")
    if args.expect_sha256:
        require(digest == args.expect_sha256, "seed ZIP differs from the one verified earlier in this run")
    root_name = name[:-4]
    work = Path(args.work)
    require(not work.exists() or not any(work.iterdir()), f"{work} must be new or empty")
    tree = work / "seed" / root_name
    with zipfile.ZipFile(zip_path) as archive:
        members = zip_members(archive, root_name)
        manifest = json.loads(archive.read(members["RELEASE.json"]).decode("utf-8")) if "RELEASE.json" in members else None
        require(isinstance(manifest, dict), "seed has no RELEASE.json")
        exes = check_manifest(manifest, version, signing=False, official=args.mode == "official")
        require(set(members) == set(manifest["files"]) | {"RELEASE.json"}, "ZIP entries differ from RELEASE.json")
        check_modes(members)
        tree.mkdir(parents=True)
        extract(archive, members, root_name, tree)
    for rel, want in manifest["files"].items():
        require(sha256_file(tree / rel) == want, f"seed member does not match RELEASE.json: {rel}")
    require(set(tree_files(tree)) == set(members), "extracted seed tree differs from the ZIP")
    check_binaries(tree, manifest["files"], exes)
    check_sources(tree, manifest, exes)
    pe_info = {rel: pe_unsigned_info((tree / rel).read_bytes(), rel) for rel in exes}
    require(pe_info["ui/Metro.exe"]["unsigned_sha256"] == STOCK_RENDERER_SHA256, "renderer hash mismatch")
    scripts = {}
    for script in SCRIPTS:
        data = regular(Path(args.scripts_dir) / script).read_bytes()
        scripts[script] = script_unsigned_info(data, script)
    summary = {"version": version, "root": root_name, "mode": args.mode, "exes": list(exes), "seed_zip": name, "seed_sha256": digest,
               "manifest": manifest, "modes": {rel: info.external_attr for rel, info in members.items()},
               "unsigned": pe_info, "scripts": scripts}
    (work / "seed.json").write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(digest)


def release_context(env, head, version, root=Path(__file__).resolve().parents[2]):
    """Fail unless `env` (a GitHub Actions environment) is the official release context."""
    def bad(what):
        return f"official Windows signing refused: {what} (only a push to protected {OFFICIAL_REF} of " \
               f"{OFFICIAL_REPOSITORY} from ci.yml is official)"
    require(SEMVER.fullmatch(version or ""), bad("invalid version input"))
    require(env.get("GITHUB_EVENT_NAME") == "push", bad(f"event is {env.get('GITHUB_EVENT_NAME')!r}"))
    require(env.get("GITHUB_REPOSITORY") == OFFICIAL_REPOSITORY
            and env.get("GITHUB_REPOSITORY_ID") == OFFICIAL_REPOSITORY_ID, bad("another repository"))
    require(env.get("GITHUB_REF") == OFFICIAL_REF and env.get("GITHUB_REF_TYPE") == "branch",
            bad(f"ref is {env.get('GITHUB_REF')!r}"))
    require(env.get("GITHUB_REF_PROTECTED") == "true", bad("the branch is not protected"))
    require(env.get("GITHUB_WORKFLOW_REF") == OFFICIAL_WORKFLOW_REF, bad("the top-level workflow is not ci.yml on master"))
    sha = env.get("GITHUB_SHA", "")
    require(SHA.fullmatch(sha), bad("no commit SHA"))
    require(head == sha, bad("the checked-out commit is not the pushed commit"))
    try:
        event = json.loads(Path(env.get("GITHUB_EVENT_PATH", "")).read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        raise Failure(bad("unreadable event payload")) from error
    repository = event.get("repository") if isinstance(event, dict) else None
    require(isinstance(repository, dict) and event.get("deleted") is False and event.get("ref") == OFFICIAL_REF
            and event.get("after") == sha
            and repository.get("full_name") == OFFICIAL_REPOSITORY
            and repository.get("id") == int(OFFICIAL_REPOSITORY_ID),
            bad("the push event is a deletion or does not match the commit"))
    cargo = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    app = json.loads((root / "apps/desktop/nsh.json").read_text(encoding="utf-8"))
    require(cargo["workspace"]["package"]["version"] == app["version"] == version,
            bad("the input version must exactly match Cargo.toml and apps/desktop/nsh.json"))
    return sha


def check_release_context(args):
    print(release_context(os.environ, args.head, args.version))


def check_signing_environment(args):
    """The GitHub environment itself must reject every deployment except the master branch."""
    environment = json.loads(Path(args.environment).read_text(encoding="utf-8"))
    policies = json.loads(Path(args.policies).read_text(encoding="utf-8"))
    policy = environment.get("deployment_branch_policy") or {}
    rules = policies.get("branch_policies") or []
    require(environment.get("name") == "windows-release-signing"
            and policy.get("protected_branches") is False and policy.get("custom_branch_policies") is True
            and policies.get("total_count") == len(rules) == 1
            and rules[0].get("name") == "master" and rules[0].get("type") == "branch",
            "windows-release-signing needs one custom deployment rule: branch master, no tags")


def load_summary(work):
    summary = json.loads((Path(work) / "seed.json").read_text(encoding="utf-8"))
    require(summary["manifest"]["files"]["ui/Metro.exe"] == STOCK_RENDERER_SHA256, "seed summary renderer mismatch")
    return summary


def prepare(args):
    """Copy the verified seed and the checkout scripts into an allowlist-only signing tree."""
    work = Path(args.work)
    summary = load_summary(work)
    sign = work / "sign"
    require(not sign.exists(), f"{sign} already exists")
    shutil.copytree(work / "seed" / summary["root"], sign / summary["root"])
    (sign / "scripts").mkdir()
    for script in SCRIPTS:
        src = regular(Path(args.scripts_dir) / script)
        require(sha256_file(src) == summary["scripts"][script]["unsigned_sha256"], f"{script} changed after verification")
        shutil.copyfile(src, sign / "scripts" / script)
    targets = [sign / summary["root"] / rel for rel in summary["exes"]] + [sign / "scripts" / s for s in SCRIPTS]
    (work / "to-sign.txt").write_text("".join(f"{p.resolve()}\n" for p in targets), encoding="utf-8")
    print(work / "to-sign.txt")


# --------------------------------------------------------------------------- finalize

def write_zip(stage, output, root, modes):
    files = tree_files(stage)
    require(set(files) == set(modes), "final tree differs from the seed's file list")
    with zipfile.ZipFile(output, "x") as result:
        for rel in sorted(files):
            info = zipfile.ZipInfo(f"{root}/{rel}")
            info.create_system = 3
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = modes[rel]
            with open(files[rel], "rb") as src, result.open(info, "w") as dst:
                shutil.copyfileobj(src, dst)


def finalize(args):
    official = args.mode == "official"
    require(SHA.fullmatch(args.commit or ""), "--commit must be the 40-hex commit being built")
    require(not official or args.publisher_cn, "official mode needs the expected publisher CN")
    work = Path(args.work)
    summary = load_summary(work)
    if official:
        release_context(os.environ, args.commit, summary["version"])
    require(summary["mode"] == args.mode, "the seed was verified for a different mode")
    exes = tuple(summary["exes"])
    root = summary["root"]
    seed = work / "seed" / root
    signed = work / "sign" / root
    out = Path(args.out_dir)
    out.mkdir(parents=True, exist_ok=True)
    require(not any(out.iterdir()), f"{out} must be empty")
    manifest = dict(summary["manifest"])
    files = dict(manifest["files"])
    record = {}
    current = tree_files(signed)
    require(set(current) == set(summary["modes"]), "files were added to or removed from the signing tree")
    for rel, path in current.items():
        if rel in exes:
            continue
        require(sha256_file(path) == sha256_file(seed / rel), f"{rel} changed during signing")
    for rel in exes:
        data = (signed / rel).read_bytes()
        info = summary["unsigned"][rel]
        if official:
            _, blob = pe_split(data, info, rel)
            seen = inspect_signature(blob, rel, args.publisher_cn, args.publisher_org)
            record[rel] = {**info, "publisher_cn": seen["publisher_cn"], "timestamp": seen["timestamp"]}
        else:
            require(sha256_bytes(data) == info["unsigned_sha256"], f"{rel} must stay unsigned in preview mode")
            record[rel] = info
        files[rel] = sha256_bytes(data)
    manifest["files"] = files
    manifest["signing"] = {
        "mode": "authenticode" if official else "unsigned-preview",
        "seed_sha256": summary["seed_sha256"],
        "commit": args.commit,
        "stock_renderer_unsigned_sha256": STOCK_RENDERER_SHA256,
        "files": record,
    }
    (signed / "RELEASE.json").write_bytes((json.dumps(manifest, indent=2, sort_keys=True) + "\n").encode("utf-8"))
    name = summary["seed_zip"]
    write_zip(signed, out / name, root, summary["modes"])
    write_sidecar(out / name, name)
    for script in SCRIPTS:
        data = (work / "sign" / "scripts" / script).read_bytes()
        info = summary["scripts"][script]
        if official:
            _, blob = script_split(data, info, script)
            inspect_signature(blob, script, args.publisher_cn, args.publisher_org)
        else:
            require(sha256_bytes(data) == info["unsigned_sha256"], f"{script} must stay unsigned in preview mode")
        (out / script).write_bytes(data)
    verify_zip(out / name, summary["version"], args.mode, args.publisher_cn, args.publisher_org, expect_commit=args.commit)
    print(sha256_file(out / name))


def verify_zip(zip_path, version, mode, publisher_cn=None, publisher_org=None, tree_out=None, expect_commit=None):
    """Re-extract the final ZIP into a fresh directory and verify everything in it."""
    official = mode == "official"
    root = f"Metro-{version}-{TARGET}"
    with zipfile.ZipFile(zip_path) as archive:
        members = zip_members(archive, root)
        manifest = json.loads(archive.read(members["RELEASE.json"]).decode("utf-8"))
        exes = check_manifest(manifest, version, signing=True, official=official)
        require(set(members) == set(manifest["files"]) | {"RELEASE.json"}, "final ZIP entries differ from RELEASE.json")
        check_modes(members)
        signing = manifest["signing"]
        require(set(signing) == {"mode", "seed_sha256", "commit", "stock_renderer_unsigned_sha256", "files"}
                and SHA.fullmatch(str(signing["commit"])) and (expect_commit is None or signing["commit"] == expect_commit)
                and signing["mode"] == ("authenticode" if official else "unsigned-preview")
                and signing["stock_renderer_unsigned_sha256"] == STOCK_RENDERER_SHA256
                and set(signing["files"]) == set(exes), "invalid RELEASE.json signing record")
        dest = Path(tree_out) if tree_out else None
        if dest is None:
            holder = tempfile.TemporaryDirectory(prefix="metro-final-")
            dest = Path(holder.name)
        require(not dest.exists() or not any(dest.iterdir()), f"{dest} must be new or empty")
        dest.mkdir(parents=True, exist_ok=True)
        extract(archive, members, root, dest)
    verify_tree(dest, manifest, mode, publisher_cn, publisher_org)
    return manifest


def verify_tree(tree, manifest, mode, publisher_cn=None, publisher_org=None, allow_marker=False):
    """A ZIP extraction or an MSI-installed folder holds exactly the signed release files."""
    tree = Path(tree)
    official = mode == "official"
    present = tree_files(tree)
    if allow_marker and MARKER in present:
        require(present[MARKER].stat().st_size == 0, f"{MARKER} must be empty")
        del present[MARKER]
    require(set(present) == set(manifest["files"]) | {"RELEASE.json"},
            f"tree differs from RELEASE.json: extra {sorted(set(present) - set(manifest['files']) - {'RELEASE.json'})}, "
            f"missing {sorted(set(manifest['files']) - set(present))}")
    for rel, want in manifest["files"].items():
        require(sha256_file(present[rel]) == want, f"{rel} does not match RELEASE.json")
    on_disk = json.loads(present["RELEASE.json"].read_text(encoding="utf-8"))
    require(on_disk == manifest, "RELEASE.json in the tree differs from the archive's")
    exes = exes_in(manifest["files"], official)
    check_binaries(tree, manifest["files"], exes)
    for rel in exes:
        data = present[rel].read_bytes()
        info = manifest["signing"]["files"][rel]
        if official:
            unsigned, blob = pe_split(data, info, rel)
            inspect_signature(blob, rel, publisher_cn, publisher_org)
        else:
            unsigned = data
            require(sha256_bytes(data) == info["unsigned_sha256"], f"{rel} differs from the unsigned seed")
        if rel == "ui/Metro.exe":
            require(sha256_bytes(unsigned) == STOCK_RENDERER_SHA256,
                    "the renderer's code differs from the pinned stock NSH renderer")
    # 7-frame icon group in the signed launcher, ICO sidecar unchanged.
    require(len(package.ico_images(package.app_icon("metro.ico"))) == ICON_FRAMES, "ICO frame count changed")
    package.verify_desktop_icons(tree, True)


def verify_dist(args):
    """The whole Windows distribution directory, after signing (run on the final copy)."""
    official = args.mode == "official"
    require(not official or args.publisher_cn, "official mode needs the expected publisher CN")
    version = args.version
    dist = Path(args.dist)
    zip_name = f"Metro-{version}-{TARGET}.zip"
    msi_name = f"Metro-{version}-{TARGET}.msi" if official else f"Metro-{version}-{TARGET}.unsigned-preview.msi"
    expected = {zip_name, zip_name + ".sha256", msi_name, msi_name + ".sha256", *SCRIPTS}
    present = {p.name for p in dist.iterdir()}
    require(present == expected, f"distribution files {sorted(present)} are not the expected {sorted(expected)}")
    for name in (zip_name, msi_name):
        require(sha256_file(dist / name) == read_sidecar(dist / (name + ".sha256"), name), f"{name} does not match its .sha256")
    manifest = verify_zip(dist / zip_name, version, args.mode, args.publisher_cn, args.publisher_org, args.tree_out,
                          expect_commit=args.expect_commit)
    if args.expect_zip_sha256:
        require(sha256_file(dist / zip_name) == args.expect_zip_sha256, "ZIP differs from the one finalized in this run")
    if args.expect_seed_sha256:
        require(manifest["signing"]["seed_sha256"] == args.expect_seed_sha256, "ZIP was not built from this run's seed")
    seen = []
    sig = msi_signature(dist / msi_name)
    if official:
        require(sig is not None, f"{msi_name} is not signed")
        seen.append(inspect_signature(sig, msi_name, args.publisher_cn, args.publisher_org))
    else:
        require(sig is None, f"{msi_name} must be unsigned in preview mode")
    for script in SCRIPTS:
        data = (dist / script).read_bytes()
        source = regular(Path(args.scripts_dir) / script).read_bytes()
        info = script_unsigned_info(source, script)
        if official:
            _, blob = script_split(data, info, script)
            seen.append(inspect_signature(blob, script, args.publisher_cn, args.publisher_org))
        else:
            require(data == source, f"{script} must be the unchanged source copy in preview mode")
    if official:
        tree = Path(args.tree_out)
        for rel in exes_in(manifest["files"], True):
            _, blob = pe_split((tree / rel).read_bytes(), manifest["signing"]["files"][rel], rel)
            seen.append(inspect_signature(blob, rel, args.publisher_cn, args.publisher_org))
        same_publisher(seen)
    sums = "".join(f"{sha256_file(dist / n)}  {n}\n" for n in sorted(expected) if not n.endswith(".sha256"))
    if args.sums_out:
        Path(args.sums_out).write_bytes(sums.encode("ascii"))
    sys.stdout.write(sums)


def extract_final(args):
    require(sha256_file(args.zip) == args.expect_sha256, "final ZIP differs from the one finalized in this run")
    verify_zip(args.zip, args.version, args.mode, args.publisher_cn, args.publisher_org, args.tree_out,
               expect_commit=args.expect_commit)
    print(Path(args.tree_out))


def same_publisher(seen):
    """Every signature in one release names the same publisher and was timestamped together."""
    require(len({(s["publisher_cn"], s["publisher_org"]) for s in seen}) == 1,
            "the release's signatures do not all name the same publisher")
    times = sorted(dt.datetime.fromisoformat(s["timestamp"]) for s in seen)
    require(times[-1] - times[0] <= MAX_TIMESTAMP_SPREAD, "the release's timestamps are not from one signing run")


def verify_installed(args):
    manifest = json.loads(regular(Path(args.manifest_from) / "RELEASE.json").read_text(encoding="utf-8"))
    check_manifest(manifest, args.version, signing=True, official=args.mode == "official")
    verify_tree(args.root, manifest, args.mode, args.publisher_cn, args.publisher_org, allow_marker=args.allow_marker)
    print(f"installed payload matches RELEASE.json ({len(manifest['files']) + 1} files)")


def verify_admin_image(args):
    """An MSI administrative image holds exactly the release tree (plus the MSI copy msiexec leaves)."""
    manifest = json.loads(regular(Path(args.manifest_from) / "RELEASE.json").read_text(encoding="utf-8"))
    official = args.mode == "official"
    check_manifest(manifest, args.version, signing=True, official=official)
    image = Path(args.image)
    files = tree_files(image)
    roots = [rel.rsplit("/", 1)[0] if "/" in rel else "" for rel in files if rel.endswith("RELEASE.json")
             and rel.rsplit("/", 1)[-1] == "RELEASE.json"]
    require(len(roots) == 1 and roots[0], f"expected one installed Metro folder in the admin image, found {roots}")
    prefix = roots[0] + "/"
    outside = sorted(rel for rel in files if not rel.startswith(prefix))
    require(all("/" not in rel and rel.lower().endswith(".msi") for rel in outside),
            f"the MSI installs files outside its Metro folder: {outside}")
    verify_tree(image / roots[0], manifest, args.mode, args.publisher_cn, args.publisher_org, allow_marker=True)
    print(f"admin image: {roots[0]} matches RELEASE.json")


def delta(args):
    """Self-test helper: a real SignTool output differs from its input only in signature bytes."""
    unsigned = regular(args.unsigned).read_bytes()
    signed = regular(args.signed).read_bytes()
    if unsigned[:2] == b"MZ":
        restored, blob = pe_split(signed, pe_unsigned_info(unsigned, "unsigned"), "signed")
    else:
        restored, blob = script_split(signed, script_unsigned_info(unsigned, "unsigned"), "signed")
    require(restored == unsigned, "restored bytes differ")
    SignedData(blob, "self-test").single_signer()
    print(f"signature-only delta: {len(signed) - len(unsigned)} bytes, PKCS#7 {len(blob)} bytes")


def msi_state(args):
    blob = msi_signature(Path(args.msi))
    if args.expect == "unsigned":
        require(blob is None, f"{args.msi} already carries a signature")
    else:
        require(blob is not None, f"{args.msi} is not signed")
        SignedData(blob, "msi").single_signer()
    print(f"{Path(args.msi).name}: {args.expect}")


def inspect(args):
    path = regular(args.path)
    data = path.read_bytes()
    if data[:2] == b"MZ":
        _, security_at = pe_layout(data, path.name)
        offset, size = struct.unpack_from("<II", data, security_at)
        require(offset and size, f"{path.name}: not signed")
        blob = data[offset + 8:offset + struct.unpack_from("<I", data, offset)[0]]
        blob = parse_one(blob, 0)[0].raw
    elif data[:8] == bytes.fromhex("d0cf11e0a1b11ae1"):
        blob = msi_signature(path)
        require(blob is not None, f"{path.name}: not signed")
    else:
        match = re.search(rb"\r?\n# SIG # Begin signature block.*\Z", data, re.S)
        require(match, f"{path.name}: not signed")
        _, blob = script_split(data, {"unsigned_size": match.start(),
                                      "unsigned_sha256": sha256_bytes(data[:match.start()])}, path.name)
    print(json.dumps(inspect_signature(blob, path.name, args.publisher_cn, args.publisher_org), sort_keys=True))


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)

    def publisher(p):
        p.add_argument("--publisher-cn", default=None)
        p.add_argument("--publisher-org", default=None)

    s = sub.add_parser("verify-seed")
    s.add_argument("--seed-dir", required=True)
    s.add_argument("--version", required=True)
    s.add_argument("--work", required=True)
    s.add_argument("--scripts-dir", required=True)
    s.add_argument("--mode", required=True, choices=("official", "preview"))
    s.add_argument("--expect-sha256")
    s.set_defaults(func=verify_seed)
    p = sub.add_parser("prepare")
    p.add_argument("--work", required=True)
    p.add_argument("--scripts-dir", required=True)
    p.set_defaults(func=prepare)
    f = sub.add_parser("finalize")
    f.add_argument("--work", required=True)
    f.add_argument("--mode", required=True, choices=("official", "preview"))
    f.add_argument("--out-dir", required=True)
    f.add_argument("--commit", required=True, help="the commit being built (github.sha); recorded in RELEASE.json")
    publisher(f)
    f.set_defaults(func=finalize)
    z = sub.add_parser("extract-final")
    z.add_argument("--zip", required=True)
    z.add_argument("--version", required=True)
    z.add_argument("--mode", required=True, choices=("official", "preview"))
    z.add_argument("--expect-sha256", required=True)
    z.add_argument("--tree-out", required=True)
    z.add_argument("--expect-commit")
    publisher(z)
    z.set_defaults(func=extract_final)
    d = sub.add_parser("verify-dist")
    d.add_argument("--dist", required=True)
    d.add_argument("--version", required=True)
    d.add_argument("--mode", required=True, choices=("official", "preview"))
    d.add_argument("--scripts-dir", required=True)
    d.add_argument("--tree-out", required=True)
    d.add_argument("--sums-out")
    d.add_argument("--expect-zip-sha256")
    d.add_argument("--expect-seed-sha256")
    d.add_argument("--expect-commit")
    publisher(d)
    d.set_defaults(func=verify_dist)
    i = sub.add_parser("verify-installed")
    i.add_argument("--root", required=True)
    i.add_argument("--manifest-from", required=True, help="verified extraction of the final ZIP")
    i.add_argument("--version", required=True)
    i.add_argument("--mode", required=True, choices=("official", "preview"))
    i.add_argument("--allow-marker", action="store_true", help=f"accept an empty {MARKER} (MSI local-runner choice)")
    publisher(i)
    i.set_defaults(func=verify_installed)
    a = sub.add_parser("verify-admin-image")
    a.add_argument("--image", required=True)
    a.add_argument("--manifest-from", required=True)
    a.add_argument("--version", required=True)
    a.add_argument("--mode", required=True, choices=("official", "preview"))
    publisher(a)
    a.set_defaults(func=verify_admin_image)
    t = sub.add_parser("delta", help="self-test: compare a SignTool output with its unsigned input")
    t.add_argument("--unsigned", required=True)
    t.add_argument("--signed", required=True)
    t.set_defaults(func=delta)
    m = sub.add_parser("msi-state")
    m.add_argument("--msi", required=True)
    m.add_argument("--expect", required=True, choices=("unsigned", "signed"))
    m.set_defaults(func=msi_state)
    c = sub.add_parser("release-context", help="fail unless this is the official master-branch release context")
    c.add_argument("--version", required=True)
    c.add_argument("--head", required=True, help="the checked-out commit (git rev-parse HEAD)")
    c.set_defaults(func=check_release_context)
    e = sub.add_parser("signing-environment", help="check the GitHub signing environment deployment policy")
    e.add_argument("--environment", required=True)
    e.add_argument("--policies", required=True)
    e.set_defaults(func=check_signing_environment)
    n = sub.add_parser("inspect", help="structural Authenticode checks of one signed file")
    n.add_argument("path")
    publisher(n)
    n.set_defaults(func=inspect)
    args = parser.parse_args(argv)
    for key in ("publisher_cn", "publisher_org"):
        if getattr(args, key, None) == "":
            setattr(args, key, None)
    try:
        args.func(args)
    except (ValueError, OSError, KeyError, IndexError, TypeError, struct.error, zipfile.BadZipFile) as error:
        parser.exit(1, f"winsign: {error}\n")


if __name__ == "__main__":
    main()
