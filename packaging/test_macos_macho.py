"""Tests for macos_macho.py (any host; no codesign).

Synthetic thin Mach-O executables are signed by a small test-only ad hoc
signer that writes what `codesign --force --sign -` writes for a bundle
(embedded signature: CodeDirectory, requirements, empty CMS wrapper). The
signer is checked against the real stock NSH runtimes when they are present:
set METRO_NSH_MACOS_RUNTIMES to a folder with arm64/ and x86_64/ copies of the
official nsh-macos-<arch>.tar.gz extracted (nsh/runtime/macos/nsh-macos-app).
"""

import hashlib
import os
from pathlib import Path
import struct
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
import macos_macho as mm  # noqa: E402

IDENT = "dev.metro.desktop.ui"
INFO = b"<plist>MetroUI</plist>"
SEAL = b"<plist>CodeResources</plist>"


def _set(buf, off, fmt, *values):
    struct.pack_into(fmt, buf, off, *values)


def make_unsigned(code=b"\x90" * 9000, page=0x4000, cputype=0x0100000C, extra_lc=b""):
    """A thin executable: __TEXT, __LINKEDIT, LC_CODE_SIGNATURE (filled in by sign())."""
    seg = lambda name, vmaddr, vmsize, fileoff, filesize: struct.pack(  # noqa: E731
        "<II16sQQQQiiII", mm.LC_SEGMENT_64, 72, name, vmaddr, vmsize, fileoff, filesize, 5, 5, 0, 0)
    sizeofcmds = 72 * 2 + 16 + len(extra_lc)
    text_size = -(-(32 + sizeofcmds + len(code)) // page) * page
    linkedit_data = b"symbols-and-strings\0" * 3
    dataoff = (text_size + len(linkedit_data) + 15) // 16 * 16
    cmds = (seg(b"__TEXT", 0x100000000, text_size, 0, text_size)
            + seg(b"__LINKEDIT", 0x100000000 + text_size, 0, text_size, 0)
            + struct.pack("<IIII", mm.LC_CODE_SIGNATURE, 16, dataoff, 0) + extra_lc)
    header = struct.pack("<IiiIIIII", mm.MH_MAGIC_64, cputype, 0, mm.MH_EXECUTE, 3 + bool(extra_lc),
                         sizeofcmds, 0x200085, 0)
    body = bytearray(header + cmds + code)
    body += bytes(text_size - len(body)) + linkedit_data
    body += bytes(dataoff - len(body))
    return bytes(body), page


def code_directory(body, ident, page, n_special, specials, flags=mm.CS_ADHOC, hash_type=2):
    hasher, size = mm.HASHES[hash_type]
    n_code = -(-len(body) // page)
    ident_b = ident.encode() + b"\0"
    ident_off = 88
    hash_off = ident_off + len(ident_b) + n_special * size
    length = hash_off + n_code * size
    cd = bytearray(length)
    _set(cd, 0, ">IIIIIIIIIBBBBI", mm.CSMAGIC_CODEDIRECTORY, length, 0x20400, flags, hash_off, ident_off,
         n_special, n_code, len(body), size, hash_type, 0, page.bit_length() - 1, 0)
    cd[ident_off:ident_off + len(ident_b)] = ident_b
    for slot, data in specials.items():
        cd[hash_off - slot * size:hash_off - (slot - 1) * size] = hasher(data).digest()
    for i in range(n_code):
        cd[hash_off + i * size:hash_off + (i + 1) * size] = hasher(body[i * page:(i + 1) * page]).digest()
    return bytes(cd)


def sign(macho, ident=IDENT, info=INFO, seal=SEAL, n_special=5, flags=mm.CS_ADHOC, extra_blobs=(),
         wrapper=b"", hash_types=(2,)):
    """Replace the signature as `codesign --force --sign - --timestamp=none` does.

    `macho` is a signed executable, or (unsigned body, page size) from make_unsigned()."""
    if isinstance(macho, tuple):
        body, page = bytearray(macho[0]), macho[1]
    else:
        m = mm.MachO(macho)
        body, page = bytearray(m.data[:m.dataoff]), 1 << _page_log2(m)
    reqs = struct.pack(">III", mm.CSMAGIC_REQUIREMENTS, 12, 0)
    cms = struct.pack(">II", mm.CSMAGIC_BLOBWRAPPER, 8 + len(wrapper)) + wrapper
    specials = {1: info, 2: reqs, 3: seal} if n_special >= 3 else {}

    def build(body):
        blobs = [(0, code_directory(bytes(body), ident, page, n_special, specials, flags, hash_types[0]))]
        blobs += [(0x1000 + i, code_directory(bytes(body), ident, page, n_special, specials, flags, t))
                  for i, t in enumerate(hash_types[1:])]
        blobs += [(2, reqs), *extra_blobs, (0x10000, cms)]
        return _superblob(blobs)

    sig_off, le_off = _offsets(body)
    for _ in range(2):  # the sizes go into page 0, which the CodeDirectory hashes
        sig = build(body)
        dataoff = struct.unpack_from("<I", body, sig_off + 8)[0]
        fileoff = struct.unpack_from("<Q", body, le_off + 40)[0]
        _set(body, sig_off + 12, "<I", len(sig))
        filesize = dataoff + len(sig) - fileoff
        _set(body, le_off + 32, "<Q", -(-filesize // page) * page)
        _set(body, le_off + 48, "<Q", filesize)
    return bytes(body) + build(body)


def _page_log2(m):
    for _slot, magic, blob in m.blobs():
        if magic == mm.CSMAGIC_CODEDIRECTORY:
            return blob[39]
    raise AssertionError("no CodeDirectory")


def _offsets(body):
    ncmds, sizeofcmds = struct.unpack_from("<II", body, 16)
    off, sig_off, le_off = 32, None, None
    for _ in range(ncmds):
        cmd, size = struct.unpack_from("<II", body, off)
        if cmd == mm.LC_CODE_SIGNATURE:
            sig_off = off
        if cmd == mm.LC_SEGMENT_64 and body[off + 8:off + 24].rstrip(b"\0") == b"__LINKEDIT":
            le_off = off
        off += size
    return sig_off, le_off


def _superblob(blobs):
    header = 12 + 8 * len(blobs)
    index, payload, off = b"", b"", header
    for slot, blob in blobs:
        index += struct.pack(">II", slot, off)
        payload += blob
        off += len(blob)
    return struct.pack(">III", mm.CSMAGIC_EMBEDDED_SIGNATURE, off, len(blobs)) + index + payload


def stock_and_packaged(**packaged_kwargs):
    stock = sign(make_unsigned(), ident="nsh-macos-app-5555494400000000", n_special=2)
    return stock, sign(stock, **packaged_kwargs)


class SameCodeTests(unittest.TestCase):
    def test_resigned_stock_runtime_is_accepted(self):
        stock, packaged = stock_and_packaged()
        self.assertNotEqual(stock, packaged)
        self.assertNotEqual(len(stock), len(packaged))  # the bundle signature is larger
        mm.same_code(packaged, stock)
        mm.verify_adhoc_signature(packaged, IDENT, INFO, SEAL)
        # x86_64 pages and an extra SHA-1 CodeDirectory, as older deployment targets get.
        stock = sign(make_unsigned(page=0x1000, cputype=0x01000007), ident="nsh", n_special=2)
        packaged = sign(stock, hash_types=(2, 1))
        mm.same_code(packaged, stock)
        mm.verify_adhoc_signature(packaged, IDENT, INFO, SEAL)

    def test_any_change_outside_the_signature_is_refused(self):
        stock, packaged = stock_and_packaged()
        m = mm.MachO(packaged)
        cases = {
            "code byte": lambda b: b.__setitem__(m.dataoff // 2, b[m.dataoff // 2] ^ 1),
            "linkedit byte": lambda b: b.__setitem__(m.dataoff - 70, b[m.dataoff - 70] ^ 1),
            "header flags": lambda b: _set(b, 24, "<I", 0x200087),
            "signature offset": lambda b: _set(b, m.signature[0] + 8, "<I", m.dataoff - 16),
            "linkedit fileoff": lambda b: _set(b, m.linkedit[0] + 40, "<Q", m.linkedit[3] - 16),
        }
        for name, mutate in cases.items():
            body = bytearray(packaged)
            mutate(body)
            with self.subTest(name), self.assertRaises(mm.MachOError):
                mm.same_code(sign_raw(body), stock)

    def test_other_binaries_and_appended_data_are_refused(self):
        stock, packaged = stock_and_packaged()
        trimmed = sign(make_unsigned(code=b"\x90" * 8000))  # a runner built for the app, not the stock one
        extra_lc = sign(make_unsigned(extra_lc=struct.pack("<II", 0x2A, 16) + bytes(8)))  # another load command
        other_cpu = sign(make_unsigned(cputype=0x01000007))
        for name, candidate in (("trimmed runner", trimmed), ("extra load command", extra_lc),
                                ("other CPU", other_cpu), ("appended bytes", packaged + b"\0" * 16),
                                ("unsigned", make_unsigned()[0]), ("not Mach-O", b"MZ" + bytes(64)),
                                ("fat header", struct.pack(">II", 0xCAFEBABE, 2) + bytes(64))):
            with self.subTest(name), self.assertRaises(mm.MachOError):
                mm.same_code(candidate, stock)
        # The stock side is checked too: a different "stock" file is not trusted.
        with self.assertRaises(mm.MachOError):
            mm.same_code(packaged, trimmed)


def sign_raw(body):
    """Re-sign mutated bytes so only the mutation (not a stale seal) differs."""
    try:
        return sign(bytes(body))
    except (mm.MachOError, struct.error):
        return bytes(body)  # no longer parses: same_code must refuse it as it is


class SignatureTests(unittest.TestCase):
    def test_seal_must_match_the_bundle_identifier_and_code(self):
        _, packaged = stock_and_packaged()
        for name, args in (("identifier", ("dev.metro.desktop", INFO, SEAL)),
                           ("Info.plist", (IDENT, INFO + b" ", SEAL)),
                           ("CodeResources", (IDENT, INFO, SEAL + b" "))):
            with self.subTest(name), self.assertRaises(mm.MachOError):
                mm.verify_adhoc_signature(packaged, *args)
        # Code changed after signing: the page hashes no longer match.
        m = mm.MachO(packaged)
        tampered = bytearray(packaged)
        tampered[m.dataoff // 2] ^= 1
        with self.assertRaisesRegex(mm.MachOError, "code page"):
            mm.verify_adhoc_signature(bytes(tampered), IDENT, INFO, SEAL)

    def test_only_a_plain_bundle_ad_hoc_signature_is_accepted(self):
        stock, _ = stock_and_packaged()
        entitlements = struct.pack(">II", 0xFADE7171, 8 + 7) + b"<plist>"
        cases = {
            "not bundle-sealed": dict(n_special=2),
            "linker-signed": dict(flags=mm.CS_ADHOC | mm.CS_LINKER_SIGNED),
            "not ad hoc": dict(flags=0),
            "entitlements": dict(extra_blobs=[(5, entitlements)]),
            "certificate": dict(wrapper=b"\x30\x82cms"),
            "SHA-1 only": dict(hash_types=(1,)),
        }
        for name, kwargs in cases.items():
            with self.subTest(name), self.assertRaises(mm.MachOError):
                mm.verify_adhoc_signature(sign(stock, **kwargs), IDENT, INFO, SEAL)
        # The stock runtime's own signature is not a bundle seal for MetroUI.app.
        with self.assertRaises(mm.MachOError):
            mm.verify_adhoc_signature(stock, IDENT, INFO, SEAL)


class PackageIntegrationTests(unittest.TestCase):
    """package.stock_renderer: the check package.py runs on the inner MetroUI.app."""

    def bundle(self, root, executable, info=INFO, seal=SEAL):
        contents = root / "MetroUI.app/Contents"
        (contents / "MacOS").mkdir(parents=True)
        (contents / "_CodeSignature").mkdir()
        (contents / "MacOS/Metro").write_bytes(executable)
        (contents / "Info.plist").write_bytes(info)
        (contents / "_CodeSignature/CodeResources").write_bytes(seal)
        return contents

    def test_accepts_only_the_resigned_pinned_runtime(self):
        import tempfile
        import package
        stock, packaged = stock_and_packaged()
        trimmed = sign(make_unsigned(code=b"\xcc" * 9000))
        for name, executable, info, ok in (
            ("re-signed stock", packaged, INFO, True),
            ("app-specific runner", trimmed, INFO, False),
            ("stock with its own upstream signature", stock, INFO, False),
            ("Info.plist edited after signing", packaged, INFO + b"\n", False),
        ):
            with self.subTest(name), tempfile.TemporaryDirectory() as d:
                root = Path(d)
                (root / "stock").write_bytes(stock)
                contents = self.bundle(root, executable, info)
                def call(contents=contents, root=root):
                    package.stock_renderer(contents / "MacOS/Metro", root / "stock", contents, "macos-arm64")
                if ok:
                    call()
                else:
                    with self.assertRaisesRegex(ValueError, "inner executable must be the stock NSH .* NSH_RUNTIME"):
                        call()


RUNTIMES = os.environ.get("METRO_NSH_MACOS_RUNTIMES")


@unittest.skipUnless(RUNTIMES, "set METRO_NSH_MACOS_RUNTIMES to the extracted official NSH macOS runtimes")
class RealStockRuntimeTests(unittest.TestCase):
    def runtimes(self):
        import package
        for target, arch in (("macos-arm64", "arm64"), ("macos-x64", "x86_64")):
            data = (Path(RUNTIMES) / arch / "nsh/runtime/macos/nsh-macos-app").read_bytes()
            self.assertEqual(hashlib.sha256(data).hexdigest(), package.NSH_MACOS_APPS[target][0])
            yield target, data

    def test_resigned_official_runtime_passes_and_one_flipped_byte_fails(self):
        for target, stock in self.runtimes():
            with self.subTest(target):
                m = mm.MachO(stock)
                # The official runtime is itself ad hoc signed (identifier nsh-macos-app-<uuid>).
                cd = mm.code_directory(next(b for s, mg, b in m.blobs() if s == 0), target)
                self.assertTrue(cd["identifier"].startswith("nsh-macos-app-"))
                self.assertEqual(cd["flags"], mm.CS_ADHOC)
                packaged = sign(stock)
                mm.same_code(packaged, stock)
                mm.verify_adhoc_signature(packaged, IDENT, INFO, SEAL)
                bad = bytearray(packaged)
                bad[0x4000 + 123] ^= 0xFF
                with self.assertRaises(mm.MachOError):
                    mm.same_code(sign(bytes(bad)), stock)


if __name__ == "__main__":
    unittest.main()
