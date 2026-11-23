"""Mach-O checks for the inner NSH app of the macOS package. Standard library only.

`nsh package --target macos` copies the stock NSH release runner
(runtime/macos/nsh-macos-app, pinned by digest in package.NSH_MACOS_APPS)
into MetroUI.app and re-signs the bundle ad hoc. The stock runner already
carries an ad hoc signature, so the packaged executable differs from it only
in its code signature: the LC_CODE_SIGNATURE size, the __LINKEDIT segment
sizes that cover the signature, and the signature bytes themselves.

Trust comes only from the pinned stock bytes, never from the file checked:

  same_code(packaged, stock)
      The packaged file equals the stock file byte for byte up to the
      signature's offset, with only those three header fields masked; both
      end exactly where their signature ends. Any change to code, data, load
      commands, linked libraries, or anything appended, fails.

  verify_adhoc_signature(packaged, identifier, info_plist, code_resources)
      Recomputes, from the packaged bytes, what codesign sealed: every code
      page hash of every CodeDirectory over exactly the bytes before the
      signature, the Info.plist and CodeResources special slots, the bundle
      identifier, ad hoc (no team, no certificate) and no entitlements.

This is a structural check on any host. Only `codesign --verify` on macOS
says whether macOS accepts the signature.
"""

import hashlib
import math
import struct

MH_MAGIC_64 = 0xFEEDFACF
MH_EXECUTE = 2
LC_SEGMENT_64 = 0x19
LC_CODE_SIGNATURE = 0x1D
CSMAGIC_EMBEDDED_SIGNATURE = 0xFADE0CC0
CSMAGIC_CODEDIRECTORY = 0xFADE0C02
CSMAGIC_REQUIREMENTS = 0xFADE0C01
CSMAGIC_BLOBWRAPPER = 0xFADE0B01
CSMAGIC_ENTITLEMENTS = (0xFADE7171, 0xFADE7172)
CSSLOT_CODEDIRECTORY = 0
CSSLOT_ALTERNATE = range(0x1000, 0x1005)
CS_ADHOC = 0x2
CS_LINKER_SIGNED = 0x20000
SLOT_INFO_PLIST = 1
SLOT_RESOURCES = 3
SLOT_ENTITLEMENTS = (5, 7)
HASHES = {1: (hashlib.sha1, 20), 2: (hashlib.sha256, 32)}


class MachOError(ValueError):
    pass


def require(cond, message):
    if not cond:
        raise MachOError(message)


class MachO:
    def __init__(self, data, name="executable"):
        self.data, self.name = bytes(data), name
        require(len(data) >= 32, f"{name}: too short for a Mach-O header")
        magic, self.cputype, self.cpusubtype, filetype, ncmds, sizeofcmds, self.flags = \
            struct.unpack_from("<IiiIIII", data, 0)
        require(magic == MH_MAGIC_64, f"{name}: not a thin 64-bit little-endian Mach-O")
        require(filetype == MH_EXECUTE, f"{name}: not an executable")
        require(32 + sizeofcmds <= len(data), f"{name}: load commands run past the file")
        self.ncmds, self.sizeofcmds = ncmds, sizeofcmds
        self.linkedit = self.signature = None
        off = 32
        for _ in range(ncmds):
            require(off + 8 <= 32 + sizeofcmds, f"{name}: truncated load command")
            cmd, size = struct.unpack_from("<II", data, off)
            require(size >= 8 and size % 8 == 0 and off + size <= 32 + sizeofcmds,
                    f"{name}: malformed load command")
            if cmd == LC_SEGMENT_64 and data[off + 8:off + 24].rstrip(b"\0") == b"__LINKEDIT":
                require(self.linkedit is None, f"{name}: two __LINKEDIT segments")
                self.linkedit = (off, *struct.unpack_from("<QQQQ", data, off + 24))
            elif cmd == LC_CODE_SIGNATURE:
                require(self.signature is None and size == 16, f"{name}: malformed or repeated LC_CODE_SIGNATURE")
                self.signature = (off, *struct.unpack_from("<II", data, off + 8))
            off += size
        require(off == 32 + sizeofcmds, f"{name}: load commands do not fill sizeofcmds")
        require(self.linkedit is not None, f"{name}: no __LINKEDIT segment")
        require(self.signature is not None, f"{name}: not code signed")
        _, _, vmsize, fileoff, filesize = self.linkedit
        _, dataoff, datasize = self.signature
        # The signature is the last thing in __LINKEDIT and in the file.
        require(dataoff + datasize == len(data) == fileoff + filesize,
                f"{name}: the code signature does not end __LINKEDIT and the file")
        require(fileoff <= dataoff and vmsize >= filesize, f"{name}: inconsistent __LINKEDIT")
        self.dataoff, self.datasize = dataoff, datasize

    def canonical(self):
        """Bytes before the signature, with only the fields a re-sign rewrites zeroed."""
        out = bytearray(self.data[:self.dataoff])
        sig_off = self.signature[0]
        out[sig_off + 12:sig_off + 16] = bytes(4)            # LC_CODE_SIGNATURE datasize
        le_off = self.linkedit[0]
        out[le_off + 32:le_off + 40] = bytes(8)              # __LINKEDIT vmsize
        out[le_off + 48:le_off + 56] = bytes(8)              # __LINKEDIT filesize
        return bytes(out)

    def blobs(self):
        sb = self.data[self.dataoff:]
        require(len(sb) >= 12, f"{self.name}: truncated signature")
        magic, length, count = struct.unpack_from(">III", sb, 0)
        require(magic == CSMAGIC_EMBEDDED_SIGNATURE and 12 + 8 * count <= length <= len(sb),
                f"{self.name}: not an embedded signature")
        out = []
        for i in range(count):
            slot, off = struct.unpack_from(">II", sb, 12 + 8 * i)
            require(off + 8 <= length, f"{self.name}: signature index out of range")
            bmagic, blen = struct.unpack_from(">II", sb, off)
            require(blen >= 8 and off + blen <= length, f"{self.name}: signature blob out of range")
            out.append((slot, bmagic, sb[off:off + blen]))
        return out


def same_code(packaged, stock):
    """Raise unless `packaged` is `stock` with only its code signature replaced."""
    p, s = MachO(packaged, "packaged executable"), MachO(stock, "stock runtime")
    require((p.cputype, p.cpusubtype) == (s.cputype, s.cpusubtype), "different CPU type")
    require((p.ncmds, p.sizeofcmds, p.flags) == (s.ncmds, s.sizeofcmds, s.flags), "different load commands")
    require(p.signature[0] == s.signature[0] and p.linkedit[0] == s.linkedit[0],
            "the signature or __LINKEDIT load command moved")
    require(p.dataoff == s.dataoff, "the code ends at a different offset")
    require(p.linkedit[3] == s.linkedit[3], "__LINKEDIT starts at a different offset")
    require(p.canonical() == s.canonical(), "code, data or load commands differ from the stock runtime")
    return p


def code_directory(blob, name):
    require(len(blob) >= 44, f"{name}: truncated CodeDirectory")
    (magic, length, version, flags, hash_off, ident_off, n_special, n_code,
     code_limit, hash_size, hash_type, _platform, page_log2) = struct.unpack_from(">IIIIIIIIIBBBB", blob, 0)
    require(magic == CSMAGIC_CODEDIRECTORY and length == len(blob), f"{name}: bad CodeDirectory")
    fields = {"version": version, "flags": flags, "hash_off": hash_off, "n_special": n_special,
              "n_code": n_code, "code_limit": code_limit, "hash_size": hash_size, "hash_type": hash_type,
              "page_log2": page_log2, "scatter": 0, "team": 0, "code_limit64": 0}
    if version >= 0x20100:
        fields["scatter"] = struct.unpack_from(">I", blob, 44)[0]
    if version >= 0x20200:
        fields["team"] = struct.unpack_from(">I", blob, 48)[0]
    if version >= 0x20300:
        fields["code_limit64"] = struct.unpack_from(">Q", blob, 56)[0]
    require(ident_off < length and b"\0" in blob[ident_off:], f"{name}: bad identifier")
    fields["identifier"] = blob[ident_off:blob.index(b"\0", ident_off)].decode("utf-8", "strict")
    fields["blob"] = blob
    return fields


def verify_adhoc_signature(packaged, identifier, info_plist, code_resources):
    """Raise unless the packaged executable's ad hoc signature seals exactly these bytes."""
    m = MachO(packaged, "packaged executable")
    blobs = m.blobs()
    directories = [(slot, blob) for slot, magic, blob in blobs
                   if slot == CSSLOT_CODEDIRECTORY or slot in CSSLOT_ALTERNATE]
    require(any(slot == CSSLOT_CODEDIRECTORY for slot, _ in directories), "no CodeDirectory")
    for _slot, magic, blob in blobs:
        require(magic not in CSMAGIC_ENTITLEMENTS, "the signature carries entitlements")
        if magic == CSMAGIC_BLOBWRAPPER:
            require(len(blob) == 8, "the signature carries a certificate; expected ad hoc")
    sha256_seen = False
    for slot, blob in directories:
        cd = code_directory(blob, f"CodeDirectory {slot:#x}")
        require(cd["identifier"] == identifier, f"signed identifier is {cd['identifier']!r}, not {identifier!r}")
        require(cd["flags"] & CS_ADHOC and not cd["flags"] & CS_LINKER_SIGNED, "not a bundle ad hoc signature")
        require(cd["team"] == 0 and cd["scatter"] == 0, "unexpected team or scatter in an ad hoc signature")
        require(cd["code_limit"] == m.dataoff and cd["code_limit64"] in (0, m.dataoff),
                "the signature does not cover exactly the code before it")
        require(cd["hash_type"] in HASHES and HASHES[cd["hash_type"]][1] == cd["hash_size"],
                "unsupported code hash")
        require(cd["page_log2"] in (12, 14), "unexpected code page size")
        sha256_seen |= cd["hash_type"] == 2
        hasher, size = HASHES[cd["hash_type"]]
        page = 1 << cd["page_log2"]
        require(cd["n_code"] == math.ceil(m.dataoff / page), "wrong number of code pages")
        base = cd["hash_off"]
        require(base - cd["n_special"] * size >= 0 and base + cd["n_code"] * size <= len(blob),
                "hash slots out of range")

        def slot_hash(i, base=base, blob=blob, size=size):
            return blob[base + i * size:base + (i + 1) * size]
        for i in range(cd["n_code"]):
            want = hasher(m.data[i * page:min((i + 1) * page, m.dataoff)]).digest()
            require(slot_hash(i) == want, f"code page {i} is not sealed by the signature")
        require(cd["n_special"] >= SLOT_RESOURCES, "the signature does not seal the bundle")
        require(slot_hash(-SLOT_INFO_PLIST) == hasher(info_plist).digest(), "Info.plist is not the sealed one")
        require(slot_hash(-SLOT_RESOURCES) == hasher(code_resources).digest(),
                "CodeResources is not the sealed one")
        for ent in SLOT_ENTITLEMENTS:
            if cd["n_special"] >= ent:
                require(slot_hash(-ent) == bytes(size), "the signature seals entitlements")
    require(sha256_seen, "no SHA-256 CodeDirectory")
    return m
