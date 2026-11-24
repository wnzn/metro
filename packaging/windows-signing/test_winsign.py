"""Tests for winsign.py. Fixtures are synthetic DER/PE/CFB structures built here;
no certificate, key or signature in this file is real or usable for signing."""

import base64
import datetime as dt
import hashlib
import io
import json
from pathlib import Path
import re
import shutil
import stat
import struct
import sys
import tempfile
import unittest
from unittest.mock import patch
import zipfile

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import winsign  # noqa: E402

SHA256 = winsign.OID_SHA256


# ---------------------------------------------------------------- DER builders

def tlv(tag, body):
    n = len(body)
    if n < 0x80:
        head = bytes([n])
    else:
        raw = n.to_bytes((n.bit_length() + 7) // 8, "big")
        head = bytes([0x80 | len(raw)]) + raw
    return bytes([tag]) + head + body


def seq(*xs):
    return tlv(0x30, b"".join(xs))


def set_(*xs):
    return tlv(0x31, b"".join(xs))


def ctx(n, body):
    return tlv(0xA0 | n, body)


def oid(dotted):
    arcs = list(map(int, dotted.split(".")))
    out = bytearray()
    for i, arc in enumerate([40 * arcs[0] + arcs[1], *arcs[2:]]):
        chunk = [arc & 0x7F]
        arc >>= 7
        while arc:
            chunk.append(0x80 | (arc & 0x7F))
            arc >>= 7
        out += bytes(reversed(chunk))
    return tlv(0x06, bytes(out))


def integer(n):
    return tlv(0x02, n.to_bytes(max(1, (n.bit_length() + 8) // 8), "big"))


def octet(b):
    return tlv(0x04, b)


def alg(dotted):
    return seq(oid(dotted), tlv(0x05, b""))


def name(cn, org=None):
    rdns = [set_(seq(oid(winsign.OID_CN), tlv(0x0C, cn.encode())))]
    if org:
        rdns.append(set_(seq(oid(winsign.OID_O), tlv(0x0C, org.encode()))))
    return seq(*rdns)


def gtime(when):
    return tlv(0x18, when.strftime("%Y%m%d%H%M%SZ").encode())


def utctime(when):
    return tlv(0x17, when.strftime("%y%m%d%H%M%SZ").encode())


T0 = dt.datetime(2026, 1, 1, tzinfo=dt.timezone.utc)
T1 = dt.datetime(2027, 1, 1, tzinfo=dt.timezone.utc)


def certificate(subject, issuer, serial, eku):
    ext = seq(oid(winsign.OID_EKU), octet(seq(*[oid(e) for e in eku])))
    tbs = seq(ctx(0, integer(2)), integer(serial), alg("1.2.840.113549.1.1.11"), issuer,
              seq(utctime(T0), utctime(T1)), subject, seq(alg("1.2.840.113549.1.1.1"), tlv(0x03, b"\0\1")),
              tlv(0xA3, seq(ext)))
    return seq(tbs, alg("1.2.840.113549.1.1.11"), tlv(0x03, b"\0sig"))


CA = name("Synthetic Test CA")


def signer_info(issuer, serial, signature, unsigned=None, digest=SHA256):
    parts = [integer(1), seq(issuer, integer(serial)), alg(digest),
             ctx(0, seq(oid("1.2.840.113549.1.9.3"), set_(oid(winsign.OID_SPC_INDIRECT)))),
             alg("1.2.840.113549.1.1.11"), octet(signature)]
    if unsigned is not None:
        parts.append(tlv(0xA1, unsigned))
    return seq(*parts)


def content_info(content_type, content, certs, signers, digests=(SHA256,), wrap_octet=False):
    body = octet(content) if wrap_octet else content
    sd = seq(integer(1), set_(*[alg(d) for d in digests]), seq(oid(content_type), ctx(0, body)),
             ctx(0, b"".join(certs)), set_(*signers))
    return seq(oid(winsign.OID_SIGNED_DATA), ctx(0, sd))


def timestamp(signature, imprint_alg=SHA256, bound=True, when=None):
    imprint = hashlib.sha256(signature if bound else b"other").digest()
    tst = seq(integer(1), oid("1.2.3.4"), seq(alg(imprint_alg), octet(imprint)), integer(7),
              gtime(when or dt.datetime(2026, 6, 1, tzinfo=dt.timezone.utc)))
    tsa = certificate(name("Synthetic TSA"), CA, 9, [winsign.OID_TIME_STAMPING])
    return content_info(winsign.OID_TSTINFO, tst, [tsa], [signer_info(CA, 9, b"tsa-signature")], wrap_octet=True)


def authenticode(cn="Metro Publisher", org="Metro Org", eku=(winsign.OID_CODE_SIGNING,), ts="ok",
                 file_digest=SHA256, digests=(SHA256,), extra_unsigned=b"", self_signed=False, signers=1):
    signature = b"S" * 256
    subject = name(cn, org)
    cert = certificate(subject, subject if self_signed else CA, 5, list(eku))
    unsigned = b""
    if ts == "ok":
        unsigned = seq(oid(winsign.OID_RFC3161), set_(timestamp(signature)))
    elif ts == "unbound":
        unsigned = seq(oid(winsign.OID_RFC3161), set_(timestamp(signature, bound=False)))
    elif ts == "sha1":
        unsigned = seq(oid(winsign.OID_RFC3161), set_(timestamp(signature, imprint_alg="1.3.14.3.2.26")))
    elif ts == "late":
        unsigned = seq(oid(winsign.OID_RFC3161), set_(timestamp(signature, when=dt.datetime(2028, 1, 1, tzinfo=dt.timezone.utc))))
    unsigned += extra_unsigned
    indirect = seq(seq(oid("1.3.6.1.4.1.311.2.1.15"), seq()), seq(alg(file_digest), octet(b"d" * 32)))
    info = signer_info(subject if self_signed else CA, 5, signature, unsigned if unsigned else None)
    return content_info(winsign.OID_SPC_INDIRECT, indirect, [cert], [info] * signers, digests)


# ---------------------------------------------------------------- PE / CFB builders

def fake_pe(size=4096, checksum=0x1234):
    data = bytearray(size)
    data[:2] = b"MZ"
    struct.pack_into("<I", data, 0x3C, 0x80)
    data[0x80:0x84] = b"PE\0\0"
    opt = 0x80 + 24
    struct.pack_into("<H", data, opt, 0x20B)
    struct.pack_into("<I", data, opt + 64, checksum)
    struct.pack_into("<I", data, opt + 108, 16)
    data[0x400:0x410] = b"code-and-data..."
    return bytes(data)


def sign_pe(data, blob, pad_inside=3):
    out = bytearray(data)
    out += b"\0" * (-len(out) % 8)
    offset = len(out)
    body = blob + b"\0" * pad_inside
    length = 8 + len(body)
    entry = struct.pack("<IHH", length, 0x0200, 0x0002) + body
    entry += b"\0" * (-len(entry) % 8)
    out += entry
    opt = struct.unpack_from("<I", out, 0x3C)[0] + 24
    struct.pack_into("<I", out, opt + 64, 0xBEEF)
    struct.pack_into("<II", out, opt + 112 + 32, offset, len(entry))
    return bytes(out)


def cfb(streams):
    """Minimal v3 compound file with root-level streams (regular sectors only)."""
    size = 512
    sectors, entries = [], []
    for stream_name, payload in streams.items():
        start = len(sectors)
        padded = payload + b"\0" * (-len(payload) % size)
        sectors += [padded[i:i + size] for i in range(0, len(padded), size)]
        entries.append((stream_name, start, len(sectors) - start, len(payload)))
    dir_sector = len(sectors)
    fat_sector = dir_sector + 1
    fat = [0xFFFFFFFF] * (size // 4)
    for _, start, count, _ in entries:
        for i in range(count):
            fat[start + i] = start + i + 1 if i < count - 1 else 0xFFFFFFFE
    fat[dir_sector] = 0xFFFFFFFE
    fat[fat_sector] = 0xFFFFFFFD

    def dirent(entry_name, kind, left, right, child, start, length):
        raw = entry_name.encode("utf-16-le") + b"\0\0"
        e = bytearray(128)
        e[:len(raw)] = raw
        struct.pack_into("<H", e, 64, len(raw))
        e[66] = kind
        struct.pack_into("<III", e, 68, left, right, child)
        struct.pack_into("<IQ", e, 116, start, length)
        return bytes(e)

    nodes = [dirent("Root Entry", 5, 0xFFFFFFFF, 0xFFFFFFFF, 1 if entries else 0xFFFFFFFF, 0xFFFFFFFE, 0)]
    for i, (stream_name, start, _, length) in enumerate(entries):
        right = i + 2 if i + 1 < len(entries) else 0xFFFFFFFF
        nodes.append(dirent(stream_name, 2, 0xFFFFFFFF, right, 0xFFFFFFFF, start, length))
    while len(nodes) < 4:
        nodes.append(b"\0" * 64 + struct.pack("<H", 0) + b"\0" * 62)
    sectors.append(b"".join(nodes))
    sectors.append(struct.pack(f"<{size // 4}I", *fat))
    header = bytearray(512)
    header[:8] = bytes.fromhex("d0cf11e0a1b11ae1")
    struct.pack_into("<HHHH", header, 24, 0x3E, 3, 0xFFFE, 9)
    struct.pack_into("<H", header, 32, 6)
    struct.pack_into("<II", header, 44, 1, dir_sector)
    struct.pack_into("<IIIII", header, 56, 4096, 0xFFFFFFFE, 0, 0xFFFFFFFE, 0)
    difat = [fat_sector] + [0xFFFFFFFF] * 108
    struct.pack_into("<109I", header, 76, *difat)
    return bytes(header) + b"".join(sectors)


# ---------------------------------------------------------------- tests

class SignatureInspection(unittest.TestCase):
    def test_valid_signature(self):
        seen = winsign.inspect_signature(authenticode(), "x", "Metro Publisher", "Metro Org")
        self.assertEqual(seen["publisher_cn"], "Metro Publisher")
        self.assertTrue(seen["timestamp"].startswith("2026-06-01"))

    def assertRejected(self, blob, fragment, cn="Metro Publisher", org=None):
        with self.assertRaises(winsign.Failure) as caught:
            winsign.inspect_signature(blob, "x", cn, org)
        self.assertIn(fragment, str(caught.exception))

    def test_rejections(self):
        self.assertRejected(authenticode(ts=None), "RFC 3161")
        self.assertRejected(authenticode(ts="unbound"), "not bound")
        self.assertRejected(authenticode(ts="sha1"), "imprint is not SHA-256")
        self.assertRejected(authenticode(ts="late"), "outside")
        self.assertRejected(authenticode(eku=("1.3.6.1.5.5.7.3.1",)), "code-signing EKU")
        self.assertRejected(authenticode(), "publisher CN", cn="Someone Else")
        self.assertRejected(authenticode(), "publisher O", org="Other Org")
        self.assertRejected(authenticode(self_signed=True), "self-signed")
        self.assertRejected(authenticode(digests=("1.3.14.3.2.26",)), "exactly SHA-256")
        self.assertRejected(authenticode(digests=(SHA256, "1.3.14.3.2.26")), "exactly SHA-256")
        self.assertRejected(authenticode(file_digest="1.3.14.3.2.26"), "file digest")
        self.assertRejected(authenticode(signers=2), "exactly one SignerInfo")
        nested = seq(oid(winsign.OID_NESTED), set_(authenticode()))
        self.assertRejected(authenticode(extra_unsigned=nested), "nested")
        legacy = seq(oid("1.2.840.113549.1.9.6"), set_(seq()))
        self.assertRejected(authenticode(extra_unsigned=legacy), "no other unsigned")

    def test_publisher_is_required_when_given_empty(self):
        with self.assertRaises(winsign.Failure):
            winsign.inspect_signature(authenticode(), "x", "", None)

    def test_garbage_is_rejected(self):
        for blob in (b"", b"\x30\x80\x00\x00", b"\x30\x05abc", authenticode() + b"x"):
            with self.assertRaises((winsign.Failure, IndexError, ValueError)):
                winsign.inspect_signature(blob, "x", "Metro Publisher")


class PortableExecutable(unittest.TestCase):
    def setUp(self):
        self.original = fake_pe()
        self.info = winsign.pe_unsigned_info(self.original, "a.exe")

    def test_round_trip_restores_exact_unsigned_bytes(self):
        signed = sign_pe(self.original, authenticode())
        restored, blob = winsign.pe_split(signed, self.info, "a.exe")
        self.assertEqual(restored, self.original)
        self.assertEqual(blob, authenticode())

    def test_unaligned_original_is_padded_with_zeros(self):
        original = fake_pe(size=4093)
        info = winsign.pe_unsigned_info(original, "a.exe")
        restored, _ = winsign.pe_split(sign_pe(original, authenticode()), info, "a.exe")
        self.assertEqual(restored, original)

    def test_code_change_is_rejected(self):
        signed = bytearray(sign_pe(self.original, authenticode()))
        signed[0x405] ^= 1
        with self.assertRaisesRegex(winsign.Failure, "bytes other than"):
            winsign.pe_split(bytes(signed), self.info, "a.exe")

    def test_unsigned_appended_or_seed_already_signed(self):
        with self.assertRaisesRegex(winsign.Failure, "not signed"):
            winsign.pe_split(self.original, self.info, "a.exe")
        signed = sign_pe(self.original, authenticode())
        with self.assertRaisesRegex(winsign.Failure, "single trailing block"):
            winsign.pe_split(signed + b"\0" * 8, self.info, "a.exe")
        with self.assertRaisesRegex(winsign.Failure, "already signed"):
            winsign.pe_unsigned_info(signed, "a.exe")

    def test_second_certificate_entry_is_rejected(self):
        signed = bytearray(sign_pe(self.original, authenticode(), pad_inside=0))
        opt = 0x80 + 24
        offset, size = struct.unpack_from("<II", signed, opt + 112 + 32)
        extra = struct.pack("<IHH", 16, 0x0200, 0x0002) + b"\x30\x06" + b"\0" * 6
        signed += extra
        struct.pack_into("<II", signed, opt + 112 + 32, offset, size + len(extra))
        with self.assertRaisesRegex(winsign.Failure, "exactly one WIN_CERT"):
            winsign.pe_split(bytes(signed), self.info, "a.exe")


class PowerShellScripts(unittest.TestCase):
    SOURCE = b"<#\n.SYNOPSIS\n  x\n#>\nWrite-Host hi\n"

    def signed(self, blob, newline=b"\r\n"):
        text = base64.b64encode(blob).decode()
        lines = "".join(f"# {text[i:i + 64]}\r\n" for i in range(0, len(text), 64)).encode()
        return self.SOURCE + newline + b"# SIG # Begin signature block\r\n" + lines + b"# SIG # End signature block\r\n"

    def test_round_trip(self):
        info = winsign.script_unsigned_info(self.SOURCE, "s.ps1")
        original, blob = winsign.script_split(self.signed(authenticode()), info, "s.ps1")
        self.assertEqual((original, blob), (self.SOURCE, authenticode()))

    def test_rejections(self):
        info = winsign.script_unsigned_info(self.SOURCE, "s.ps1")
        with self.assertRaisesRegex(winsign.Failure, "content changed"):
            winsign.script_split(self.signed(authenticode()).replace(b"hi", b"ho"), info, "s.ps1")
        with self.assertRaisesRegex(winsign.Failure, "exactly one PowerShell signature block"):
            winsign.script_split(self.SOURCE, info, "s.ps1")
        with self.assertRaisesRegex(winsign.Failure, "exactly one PowerShell signature block"):
            winsign.script_split(self.signed(authenticode()) + b"Remove-Item x\n", info, "s.ps1")
        with self.assertRaisesRegex(winsign.Failure, "already has a signature"):
            winsign.script_unsigned_info(self.signed(authenticode()), "s.ps1")
        with self.assertRaisesRegex(winsign.Failure, "plain ASCII"):
            winsign.script_unsigned_info("café".encode(), "s.ps1")

    def test_repository_scripts_are_signable_plain_ascii(self):
        for script in winsign.SCRIPTS:
            data = (HERE.parent / script).read_bytes()
            winsign.script_unsigned_info(data, script)
            self.assertNotIn(b"ExecutionPolicy Bypass", data, f"{script} must not advise bypassing the execution policy")


class CompoundFile(unittest.TestCase):
    def test_reads_signature_stream(self):
        blob = authenticode()
        data = cfb({"\x05SummaryInformation": b"s" * 600, "\x05DigitalSignature": blob + b"\0" * (4200 - len(blob))})
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "a.msi"
            path.write_bytes(data)
            self.assertEqual(winsign.msi_signature(path), blob)
            path.write_bytes(cfb({"\x05SummaryInformation": b"s" * 4100}))
            self.assertIsNone(winsign.msi_signature(path))
            path.write_bytes(b"MZ" + data[2:])
            with self.assertRaisesRegex(winsign.Failure, "not an MSI"):
                winsign.msi_signature(path)


class Archives(unittest.TestCase):
    def manifest(self, tui=False, **change):
        files = {"Metro.exe": "a" * 64, "metro-server.exe": "b" * 64, "ui/Metro.exe": winsign.STOCK_RENDERER_SHA256}
        if tui:
            files["metro-tui.exe"] = "c" * 64
        m = {"connector": "linked", "dependencies": {k: [] for k in files}, "files": files,
             "kind": "desktop", "local_runner": winsign.package.LOCAL_RUNNER, "nsh": winsign.package.NSH_VERSION,
             "target": "windows-x64", "version": "1.2.3"}
        m.update(change)
        return m

    def test_manifest(self):
        self.assertEqual(winsign.check_manifest(self.manifest(), "1.2.3", signing=False, official=False),
                         winsign.REQUIRED_EXES)
        self.assertEqual(winsign.check_manifest(self.manifest(tui=True), "1.2.3", signing=False, official=True),
                         winsign.SIGNED_EXES)
        self.assertEqual(winsign.check_manifest(self.manifest(tui=True), "1.2.3", signing=False, official=False),
                         winsign.SIGNED_EXES)
        for bad, official, msg in ((self.manifest(), True, "expected EXEs"),
                                   (self.manifest(version="1.2.4"), False, "does not describe"),
                                   (self.manifest(signing={}), False, "keys"),
                                   (self.manifest(files={"../x": "a" * 64}), False, "unsafe"),
                                   (self.manifest(local_runner="none"), False, "does not describe"),
                                   (self.manifest(dependencies={"Metro.exe": []}), False, "dependencies")):
            with self.assertRaisesRegex(winsign.Failure, msg):
                winsign.check_manifest(bad, "1.2.3", signing=False, official=official)
        broken = self.manifest()
        broken["files"] = {**broken["files"], "RELEASE.json": "a" * 64}
        with self.assertRaisesRegex(winsign.Failure, "list itself"):
            winsign.check_manifest(broken, "1.2.3", signing=False, official=False)

    def zip_with(self, entries):
        buf = io.BytesIO()
        with zipfile.ZipFile(buf, "w") as z:
            for entry_name, mode in entries:
                info = zipfile.ZipInfo(entry_name)
                info.create_system = 3
                info.external_attr = mode << 16
                z.writestr(info, b"x")
        return zipfile.ZipFile(io.BytesIO(buf.getvalue()))

    def test_zip_entries(self):
        root = "Metro-1.2.3-windows-x64"
        good = self.zip_with([(f"{root}/Metro.exe", 0o100755), (f"{root}/a.txt", 0o100644)])
        winsign.check_modes(winsign.zip_members(good, root))
        for entries, msg in (([(f"{root}/../x", 0o100644)], "unsafe"),
                             ([("Other/x", 0o100644)], "outside"),
                             ([(f"{root}/link", 0o120777)], "entry type"),
                             ([(f"{root}/a\\b", 0o100644)], "unsafe")):
            with self.assertRaisesRegex(winsign.Failure, msg):
                winsign.zip_members(self.zip_with(entries), root)
        with self.assertRaisesRegex(winsign.Failure, "file mode"):
            winsign.check_modes(winsign.zip_members(self.zip_with([(f"{root}/Metro.exe", 0o100644)]), root))

    def test_binary_allowlist(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for rel in (*winsign.SIGNED_EXES, "notes.txt", "hidden.dat"):
                (root / rel).parent.mkdir(parents=True, exist_ok=True)
                (root / rel).write_bytes(b"MZ" if rel.endswith(".exe") else b"text")
            exes = winsign.SIGNED_EXES
            winsign.check_binaries(root, [*exes, "notes.txt", "hidden.dat"], exes)
            with self.assertRaisesRegex(winsign.Failure, "allowlist"):
                winsign.check_binaries(root, [*exes, "notes.txt"], winsign.REQUIRED_EXES)
            (root / "hidden.dat").write_bytes(b"MZ\x90\x00")
            with self.assertRaisesRegex(winsign.Failure, "allowlist"):
                winsign.check_binaries(root, [*exes, "hidden.dat"], exes)
            (root / "hidden.dat").write_bytes(b"text")
            (root / "x.ps1").write_bytes(b"text")
            with self.assertRaisesRegex(winsign.Failure, "allowlist"):
                winsign.check_binaries(root, [*exes, "x.ps1"], exes)

    def test_deterministic_zip_keeps_seed_modes(self):
        with tempfile.TemporaryDirectory() as tmp:
            stage = Path(tmp) / "stage"
            (stage / "ui").mkdir(parents=True)
            (stage / "Metro.exe").write_bytes(b"MZ1")
            (stage / "ui/a.txt").write_bytes(b"t")
            modes = {"Metro.exe": (stat.S_IFREG | 0o755) << 16, "ui/a.txt": (stat.S_IFREG | 0o644) << 16}
            winsign.write_zip(stage, Path(tmp) / "a.zip", "R", modes)
            winsign.write_zip(stage, Path(tmp) / "b.zip", "R", modes)
            self.assertEqual((Path(tmp) / "a.zip").read_bytes(), (Path(tmp) / "b.zip").read_bytes())
            with zipfile.ZipFile(Path(tmp) / "a.zip") as z:
                self.assertEqual(z.namelist(), ["R/Metro.exe", "R/ui/a.txt"])
            (stage / "extra").write_bytes(b"x")
            with self.assertRaisesRegex(winsign.Failure, "differs from the seed"):
                winsign.write_zip(stage, Path(tmp) / "c.zip", "R", modes)

    def test_sidecar(self):
        with tempfile.TemporaryDirectory() as tmp:
            p = Path(tmp) / "a.zip"
            p.write_bytes(b"zip")
            digest = winsign.write_sidecar(p, "a.zip")
            self.assertEqual(winsign.read_sidecar(Path(tmp) / "a.zip.sha256", "a.zip"), digest)
            (Path(tmp) / "a.zip.sha256").write_text(f"{digest} *a.zip\n")
            with self.assertRaises(winsign.Failure):
                winsign.read_sidecar(Path(tmp) / "a.zip.sha256", "a.zip")


def jobs(text):
    """Job name -> text of that job, for the two-space-indented `jobs:` map."""
    import re
    body = text.split("\njobs:\n", 1)[1]
    parts = re.split(r"(?m)^  ([a-z][a-z0-9-]*):\n", body)
    return dict(zip(parts[1::2], parts[2::2]))


class SeedSources(unittest.TestCase):
    def test_branded_inventory_uses_checkout_icon_font_and_licenses(self):
        package = winsign.package
        source = package.RENDERER_SOURCE
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            nsh = root / "ui/nsh"
            licenses = root / "licenses"
            licenses.mkdir()
            exes = winsign.REQUIRED_EXES
            version = "1.2.3"

            package.copy(package.app_icon("metro.ico"), root / package.WINDOWS_ICON)
            for module in package.ICON_MODULES:
                text = (source / "src/ui" / module).read_text(encoding="utf-8")
                for name in re.findall(r'\./icons/([a-z0-9-]+\.svg)', text):
                    rel = Path("src/ui/icons") / name
                    package.copy(source / rel, nsh / "assets" / rel)
            font = nsh / "assets" / package.INTER_FONT
            package.copy(source / package.INTER_FONT, font)
            (nsh / "app.json").write_text(json.dumps({"name": "Metro", "id": package.APP_ID, "version": version}))
            cap = package.read_json(source / "nsh.json")["capabilities"]
            cap = {**cap, "network": {"http": [], **cap["network"]},
                   "process": {"execute": [], "exit": False, **cap["process"]}}
            (nsh / "capabilities.json").write_text(json.dumps(cap))
            (nsh / "bundle.nshc").write_bytes(b"fixture bundle")
            (root / "START.txt").write_bytes(b"fixture instructions")
            for rel in exes:
                (root / rel).write_bytes(b"MZ" + rel.encode())
            apache = b"fixture NSH license"
            (licenses / "NSH-LICENSE-APACHE.txt").write_bytes(apache)
            (licenses / "THIRD-PARTY-NOTICES.txt").write_bytes(b"fixture third-party notices")
            (licenses / "METRO-LICENSE.txt").write_bytes(b"fixture optional license")
            brand = (source / "src/ui/icons/LICENSE-brand-marks.txt").read_text(encoding="utf-8")
            (licenses / "BRAND-MARKS.txt").write_bytes(brand.replace("\n", "\r\n").encode())
            package.write_inter_notice(root)
            ofl = licenses / "INTER-OFL.txt"

            def verify():
                # Rehash even altered inputs: RELEASE.json is not a trusted license or asset pin.
                files = {rel: winsign.sha256_file(path) for rel, path in winsign.tree_files(root).items()}
                manifest = {"version": version, "files": files, "dependencies": dict.fromkeys(exes, ["kernel32.dll"])}
                winsign.check_sources(root, manifest, exes)

            with patch.object(package, "pe", return_value=["kernel32.dll"]), \
                    patch.object(package, "NSH_LICENSE_SHA", winsign.sha256_bytes(apache)), \
                    patch.object(winsign, "STOCK_RENDERER_SHA256", winsign.sha256_file(root / "ui/Metro.exe")):
                verify()
                (licenses / "METRO-LICENSE.txt").unlink()
                verify()
                ofl.write_bytes(b"wrong OFL")
                with self.assertRaisesRegex(winsign.Failure, "unexpected Inter license"):
                    verify()
                ofl.unlink()
                with self.assertRaisesRegex(winsign.Failure, r"missing \['licenses/INTER-OFL.txt'\]"):
                    verify()
                package.write_inter_notice(root)
                extra = licenses / "extra.txt"
                extra.touch()
                with self.assertRaisesRegex(winsign.Failure, r"extra \['licenses/extra.txt'\]"):
                    verify()
                extra.unlink()
                (licenses / "BRAND-MARKS.txt").write_bytes(b"wrong brand notice")
                with self.assertRaisesRegex(winsign.Failure, "brand notice differs"):
                    verify()
                (licenses / "BRAND-MARKS.txt").write_bytes(brand.encode())
                font.write_bytes(b"wrong font")
                with self.assertRaisesRegex(ValueError, "unexpected Inter font digest"):
                    verify()


class ReleaseContext(unittest.TestCase):
    SHA = "a" * 40
    VERSION = json.loads((HERE.parents[1] / "apps/desktop/nsh.json").read_text())["version"]

    def context(self, tmp, event=None, **env):
        payload = {"ref": "refs/heads/main", "after": self.SHA, "deleted": False,
                   "repository": {"full_name": "wnzn/metro", "id": 1467654868}}
        payload.update(event or {})
        path = Path(tmp) / "event.json"
        path.write_text(json.dumps(payload))
        base = {"GITHUB_EVENT_NAME": "push", "GITHUB_REPOSITORY": "wnzn/metro", "GITHUB_REF": "refs/heads/main",
                 "GITHUB_REPOSITORY_ID": "1467654868",
                "GITHUB_REF_TYPE": "branch", "GITHUB_REF_PROTECTED": "true", "GITHUB_SHA": self.SHA,
                "GITHUB_WORKFLOW_REF": "wnzn/metro/.github/workflows/ci.yml@refs/heads/main",
                "GITHUB_EVENT_PATH": str(path)}
        base.update(env)
        return base

    def test_official_main_push(self):
        with tempfile.TemporaryDirectory() as tmp:
            self.assertEqual(winsign.release_context(self.context(tmp), self.SHA, self.VERSION), self.SHA)

    def test_input_version_matches_both_files_even_for_prereleases(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            app = root / "apps/desktop/nsh.json"
            app.parent.mkdir(parents=True)
            version = "0.3.0-rc.1"
            (root / "Cargo.toml").write_text(f'[workspace.package]\nversion = "{version}"\n')
            app.write_text(json.dumps({"version": version}))
            self.assertEqual(winsign.release_context(self.context(tmp), self.SHA, version, root), self.SHA)
            with self.assertRaisesRegex(winsign.Failure, "exactly match"):
                winsign.release_context(self.context(tmp), self.SHA, "0.3.0", root)
            app.write_text(json.dumps({"version": "0.3.0"}))
            with self.assertRaisesRegex(winsign.Failure, "exactly match"):
                winsign.release_context(self.context(tmp), self.SHA, version, root)

    def test_everything_else_is_refused(self):
        other = "b" * 40
        cases = {
            "tag push": dict(GITHUB_REF="refs/tags/v0.3.0", GITHUB_REF_TYPE="tag"),
            "pull request": dict(GITHUB_EVENT_NAME="pull_request", GITHUB_REF="refs/pull/1/merge"),
            "pull_request_target": dict(GITHUB_EVENT_NAME="pull_request_target"),
            "workflow_dispatch": dict(GITHUB_EVENT_NAME="workflow_dispatch"),
            "other branch": dict(GITHUB_REF="refs/heads/dev",
                                 GITHUB_WORKFLOW_REF="wnzn/metro/.github/workflows/ci.yml@refs/heads/dev"),
            "main-like branch": dict(GITHUB_REF="refs/heads/main2"),
            "unprotected main": dict(GITHUB_REF_PROTECTED="false"),
            "fork": dict(GITHUB_REPOSITORY="someone/metro"),
            "reused repository name": dict(GITHUB_REPOSITORY_ID="123"),
            "other workflow": dict(GITHUB_WORKFLOW_REF="wnzn/metro/.github/workflows/release.yml@refs/heads/main"),
            "workflow from a tag": dict(GITHUB_WORKFLOW_REF="wnzn/metro/.github/workflows/ci.yml@refs/tags/v0.3.0"),
            "no sha": dict(GITHUB_SHA=""),
        }
        with tempfile.TemporaryDirectory() as tmp:
            for name, env in cases.items():
                with self.subTest(name), self.assertRaisesRegex(winsign.Failure, "refused"):
                    winsign.release_context(self.context(tmp, **env), self.SHA, "0.3.0")
            for name, event in {"deleted branch": {"deleted": True}, "missing deleted": {"deleted": None},
                                "event for another commit": {"after": other},
                                "event for another ref": {"ref": "refs/tags/v0.3.0"},
                                 "event from another repository": {"repository": {"full_name": "x/metro", "id": 1467654868}},
                                 "event with another repo ID": {"repository": {"full_name": "wnzn/metro", "id": 123}}}.items():
                with self.subTest(name), self.assertRaisesRegex(winsign.Failure, "refused"):
                    winsign.release_context(self.context(tmp, event), self.SHA, "0.3.0")
            with self.assertRaisesRegex(winsign.Failure, "checked-out commit"):
                winsign.release_context(self.context(tmp), other, "0.3.0")
            for version in ("", "v0.3.0", "main", "0.3"):
                with self.subTest(version), self.assertRaisesRegex(winsign.Failure, "invalid version"):
                    winsign.release_context(self.context(tmp), self.SHA, version)
            with self.assertRaisesRegex(winsign.Failure, "unreadable event"):
                winsign.release_context(self.context(tmp, GITHUB_EVENT_PATH=str(Path(tmp) / "missing.json")), self.SHA, "0.3.0")

    def test_official_finalize_refuses_outside_main(self):
        import os
        from unittest import mock
        with tempfile.TemporaryDirectory() as tmp, mock.patch.dict(os.environ, self.context(tmp, GITHUB_REF="refs/tags/v0.3.0")), \
                mock.patch.object(winsign, "load_summary", return_value={"version": "0.3.0"}):
            args = winsign.argparse.Namespace(mode="official", commit=self.SHA, publisher_cn="Metro", publisher_org=None,
                                              work=tmp, out_dir=str(Path(tmp) / "out"))
            with self.assertRaisesRegex(winsign.Failure, "refused"):
                winsign.finalize(args)


class SigningEnvironment(unittest.TestCase):
    def test_only_one_literal_main_branch_rule(self):
        with tempfile.TemporaryDirectory() as tmp:
            args = winsign.argparse.Namespace(environment=Path(tmp) / "environment", policies=Path(tmp) / "policies")
            custom = {"protected_branches": False, "custom_branch_policies": True}
            main_rule = {"name": "main", "type": "branch"}

            def check(policy, *rules, total=None):
                args.environment.write_text(json.dumps({"name": "windows-release-signing", "deployment_branch_policy": policy}))
                args.policies.write_text(json.dumps({"total_count": len(rules) if total is None else total,
                                                    "branch_policies": rules}))
                winsign.check_signing_environment(args)

            check(custom, main_rule)
            for policy, rules, total in (
                (None, [], 0), ({"protected_branches": True, "custom_branch_policies": False}, [main_rule], 1),
                (custom, [{"name": "main", "type": "tag"}], 1),
                (custom, [{"name": "main*", "type": "branch"}], 1),
                (custom, [main_rule, {"name": "v*", "type": "tag"}], 2), (custom, [main_rule], 2),
            ):
                with self.subTest(policy=policy, rules=rules, total=total), self.assertRaisesRegex(winsign.Failure, "no tags"):
                    check(policy, *rules, total=total)


class LegacyScriptCoexistence(unittest.TestCase):
    """install-metro-windows.ps1 never installs over, or touches, an MSI-managed Metro."""
    SCRIPT = HERE.parent / "install-metro-windows.ps1"

    def text(self):
        return self.SCRIPT.read_text(encoding="utf-8")

    def test_uses_the_msi_lanes_release_identity(self):
        import re
        msi = (HERE.parent / "windows-msi/build_msi.py").read_text(encoding="utf-8")
        release = msi[msi.index('"release": {'):msi.index('"fixture": {')]
        upgrade = re.search(r'"upgrade_code": "(\{[0-9A-F-]+\})"', release).group(1)
        key = re.search(r'"reg_key": r"([^"]+)"', release).group(1)
        self.assertIn(f"$MsiUpgradeCode = '{upgrade}'", self.text())
        self.assertIn(f"$MsiKey = 'HKCU:\\{key}'", self.text())

    def test_checks_happen_before_any_change(self):
        text = self.text()
        install = text[text.index("function Install-Metro"):text.index("function Uninstall-Metro")]
        self.assertLess(install.index("Get-MsiManagedMetro"), install.index("New-Item -ItemType Directory -Force -Path $Root"))
        self.assertLess(install.index("Test-OwnShortcut $existing $Root"), install.index("CreateShortcut((Get-ShortcutPath))"))
        uninstall = text[text.index("function Uninstall-Metro"):text.index("\ntry {")]
        self.assertIn("if (Test-OwnShortcut (Get-ShortcutTarget $lnk) $Root) { Remove-Item -LiteralPath $lnk -Force }", uninstall)
        self.assertEqual(uninstall.count("Remove-Item -LiteralPath $lnk"), 1)

    @unittest.skipUnless(shutil.which("pwsh"), "PowerShell 7 (pwsh) is not installed")
    def test_only_its_own_shortcut_counts(self):
        import subprocess
        command = (
            "$ast = [System.Management.Automation.Language.Parser]::ParseFile($env:METRO_SCRIPT, [ref]$null, [ref]$null);"
            "$f = $ast.Find({ param($n) $n -is [System.Management.Automation.Language.FunctionDefinitionAst] -and "
            "$n.Name -eq 'Test-OwnShortcut' }, $true); . ([scriptblock]::Create($f.Extent.Text));"
            "$root = Join-Path ([IO.Path]::GetTempPath()) 'Metro Root';"
            "@((Test-OwnShortcut (Join-Path $root 'versions/0.3.0/Metro.exe') $root),"
            "(Test-OwnShortcut (Join-Path $root 'versions/../../Metro Desktop/Metro.exe') $root),"
            "(Test-OwnShortcut (Join-Path $root 'Metro.exe') $root),"
            "(Test-OwnShortcut (Join-Path ([IO.Path]::GetTempPath()) 'Metro Rootx/versions/1/Metro.exe') $root),"
            "(Test-OwnShortcut (Join-Path ([IO.Path]::GetTempPath()) 'Metro Desktop/Metro.exe') $root),"
            "(Test-OwnShortcut '' $root)) -join ','")
        import os
        r = subprocess.run(["pwsh", "-NoProfile", "-NonInteractive", "-Command", command],
                           text=True, capture_output=True, timeout=120, env={**os.environ, "METRO_SCRIPT": str(self.SCRIPT)})
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.strip(), "True,False,False,False,False,False")


@unittest.skipUnless(shutil.which("git") and (HERE.parents[1] / ".git").exists(), "needs a git checkout")
class CrossPlatformBytes(unittest.TestCase):
    """Files hashed, signed or compared across Windows and Linux CI keep their exact
    committed bytes under core.autocrlf=true (hosted Windows runners): .gitattributes -text."""

    def git(self, *args):
        import subprocess
        return subprocess.run(["git", "-C", str(HERE.parents[1]), *args], text=True, capture_output=True,
                              check=True).stdout

    def test_hashed_inputs_are_never_converted(self):
        tracked = self.git("ls-files", "-z", "--", "apps/desktop/src/ui/icons", "apps/desktop/src/ui/fonts",
                           "apps/desktop/assets", *(f"packaging/{s}" for s in winsign.SCRIPTS)).split("\0")
        tracked = [t for t in tracked if t]
        for needed in ("apps/desktop/src/ui/icons/LICENSE-brand-marks.txt", "apps/desktop/src/ui/fonts/OFL.txt",
                       "apps/desktop/assets/metro.ico", *(f"packaging/{s}" for s in winsign.SCRIPTS)):
            self.assertIn(needed, tracked)
        self.assertTrue(any(t.endswith(".svg") for t in tracked))
        out = self.git("-c", "core.autocrlf=true", "check-attr", "-z", "text", "--", *tracked).split("\0")
        values = dict(zip(out[0::3], out[2::3]))
        self.assertEqual({t for t in tracked if values.get(t) != "unset"}, set(),
                         "these hashed inputs would get CRLF on a Windows checkout")


class Trees(unittest.TestCase):
    def test_marker_is_allowed_only_when_asked_and_empty(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "a.txt").write_bytes(b"a")
            files = {"a.txt": hashlib.sha256(b"a").hexdigest()}
            manifest = {"files": files}
            (root / "RELEASE.json").write_text(json.dumps(manifest))
            original = winsign.check_binaries
            winsign.check_binaries = lambda *a: None
            exes = winsign.exes_in
            winsign.exes_in = lambda *a: ()
            icons = winsign.package.verify_desktop_icons
            winsign.package.verify_desktop_icons = lambda *a: None
            try:
                winsign.verify_tree(root, manifest, "preview")
                (root / winsign.MARKER).write_bytes(b"")
                with self.assertRaisesRegex(winsign.Failure, "extra"):
                    winsign.verify_tree(root, manifest, "preview")
                winsign.verify_tree(root, manifest, "preview", allow_marker=True)
                (root / winsign.MARKER).write_bytes(b"x")
                with self.assertRaisesRegex(winsign.Failure, "must be empty"):
                    winsign.verify_tree(root, manifest, "preview", allow_marker=True)
            finally:
                winsign.check_binaries, winsign.exes_in = original, exes
                winsign.package.verify_desktop_icons = icons


class Workflows(unittest.TestCase):
    """Static guards on the two reusable workflows (the parts that protect signing)."""

    def read(self, name):
        """The workflow without its full-line comments."""
        text = (HERE.parents[1] / ".github/workflows" / name).read_text(encoding="utf-8")
        return "".join(line for line in text.splitlines(True) if not line.lstrip().startswith("#"))

    def check_common(self, text):
        import re
        head = text.split("\njobs:", 1)[0]
        self.assertIn("workflow_call:", head)
        for trigger in ("workflow_dispatch", "pull_request", "push:", "schedule:", "repository_dispatch", "workflow_run"):
            self.assertNotIn(trigger, head)
        self.assertRegex(head, r"(?m)^permissions:\n  contents: read\n")
        self.assertNotIn("secrets.", text)
        self.assertNotIn("secrets: inherit", text)
        for ref in re.findall(r"uses:\s*(\S+)", text):
            self.assertRegex(ref, r"^[^@]+@[0-9a-f]{40}$")
        for job, body in jobs(text).items():
            self.assertRegex(body, r"(?m)^    permissions:\n      contents: read\n", job)

    def test_distribution_never_touches_signing(self):
        text = self.read("windows-distribution.yml")
        self.check_common(text)
        for forbidden in ("environment:", "id-token", "self-hosted", "METRO_WINDOWS_SIGNING", "Invoke-MetroReleaseSigning",
                          "vars."):
            self.assertNotIn(forbidden, text)
        self.assertEqual(set(jobs(text)), {"seed", "preview"})
        self.assertIn("if: ${{ !inputs.release }}", jobs(text)["preview"])
        self.assertIn("name: unsigned-desktop-windows-x64", jobs(text)["seed"])

    def test_no_tag_or_ref_name_release_logic(self):
        for name in ("windows-distribution.yml", "windows-sign.yml"):
            text = self.read(name)
            for forbidden in ("refs/tags", "ref_name", "github.head_ref", "pull_request"):
                self.assertNotIn(forbidden, text, name)
        self.assertIn("winsign.py release-context", jobs(self.read("windows-distribution.yml"))["seed"])

    def test_signing_is_main_only_and_isolated(self):
        text = self.read("windows-sign.yml")
        self.check_common(text)
        head = text.split("\njobs:", 1)[0]
        self.assertNotRegex(head, r"(?m)^      release:")
        self.assertNotIn("New-SelfSignedCertificate", text)
        self.assertNotIn("Invoke-MetroVerifierSelfTest", text)
        all_jobs = jobs(text)
        self.assertEqual(set(all_jobs), {"guard", "sign", "msi", "sign-msi", "verify"})
        for job, body in all_jobs.items():
            privileged = job in ("sign", "sign-msi")
            self.assertEqual("environment: windows-release-signing" in body, privileged, job)
            self.assertEqual("id-token: write" in body, privileged, job)
            self.assertEqual("Invoke-MetroReleaseSigning" in body, privileged, job)
            if privileged:
                self.assertIn("Assert-MetroReleaseContext", body)
                self.assertNotIn("build-msi", body)
            else:
                self.assertIn("runs-on: ubuntu-24.04" if job in ("guard", "msi") else "runs-on: windows-2025", body)
        self.assertIn("winsign.py release-context", all_jobs["guard"])
        self.assertIn("winsign.py signing-environment", all_jobs["guard"])
        guard_if = ("if: >- github.event_name == 'push' && github.repository == 'wnzn/metro' && "
                    "github.repository_id == '1467654868' && github.ref == 'refs/heads/main' && "
                    "github.ref_type == 'branch' && github.ref_protected && toJSON(github.event.deleted) == 'false' && "
                    "github.event.after == github.sha && github.workflow_ref == 'wnzn/metro/.github/workflows/ci.yml@refs/heads/main'")
        for job in ("sign", "sign-msi"):
            # Job-level, so no runner (self-hosted included) is assigned outside the official context.
            self.assertIn(guard_if, " ".join(all_jobs[job].split()))
            self.assertLess(all_jobs[job].index("Assert-MetroReleaseContext"), all_jobs[job].index("Invoke-MetroReleaseSigning"))
            self.assertIn("--commit $env:GITHUB_SHA" if job == "sign" else "Assert-MetroReleaseContext", all_jobs[job])
        self.assertIn("--expect-commit $env:GITHUB_SHA", all_jobs["verify"])
        self.assertIn("name: dist-desktop-windows-x64", all_jobs["verify"])

    def test_module_allowlist_matches_winsign(self):
        module = (HERE / "MetroWindowsSigning.psm1").read_text(encoding="utf-8")
        for exe in winsign.SIGNED_EXES:
            self.assertIn(exe.replace("/", "\\\\").replace(".", "\\."), module)
        for script in winsign.SCRIPTS:
            self.assertIn(script.removesuffix(".ps1"), module)


if __name__ == "__main__":
    unittest.main()
