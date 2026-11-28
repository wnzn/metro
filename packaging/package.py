#!/usr/bin/env python3
"""Assemble release archives from already-built, target-specific inputs."""

import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path
import re
import runpy
import shutil
import stat
import struct
import subprocess
import sys
import tarfile
import tempfile
import xml.etree.ElementTree as ET
import zipfile


NSH_VERSION = "0.6.1"
NSH_APPS = {
    "windows": ("nsh-windows-app.exe", "3cde53d3a3d7d7bfc5eb30c670739116629034d3c3da89232c1ccbd90e01fd0f"),
    "linux": ("nsh-linux-app", "0a382b9e76ae45b5a28afc7d9ba0ea169e8390c8a21b94e74874907907d6d517"),
}
# Stock macOS runtimes from nsh-macos-<arch>.tar.gz (runtime/macos/nsh-macos-app),
# already ad hoc signed upstream. With NSH_RUNTIME set to that runtime root,
# `nsh package --target macos` copies this file unchanged into the .app and
# re-signs the bundle ad hoc (without NSH_RUNTIME, a Mac with cargo builds an
# app-specific runner instead). macos_macho.py compares the packaged copy with
# these bytes outside the code signature and checks the new seal.
NSH_MACOS_APPS = {
    "macos-arm64": ("7b972892cf7d368abe31c1ce7d5ecc4f0ef5972e0aa5ca77048f13e75ca371b5", 0x0100000C),
    "macos-x64": ("4fef59fd1f8813261e81326946750f5dc41302b6631f6dc75b30b3348be6ea89", 0x01000007),
}
MACOS_UI_ID = "dev.metro.desktop.ui"
# RELEASE.json capability: this launcher acts on the installer's marker.
LOCAL_RUNNER = "installer-marker"
# The terminal client inside each desktop archive (listed in RELEASE.json "files").
TUI_NAMES = {"linux": "metro-tui", "windows": "metro-tui.exe", "macos": "Contents/MacOS/metro-tui"}
MACOS_MIN = "14.0"
NSH_LICENSE_SHA = "ec75427ad955e3c740f22e7dc3f6db6fc5b2cc1d9135c2af91cece8dd47a2d38"
INTER_FONT = Path("src/ui/fonts/InterVariable.ttf")
INTER_FONT_SHA = "29160a80ff49ddcab2c97711247e08b1fab27a484a329ce8b813d820dc559031"
INTER_OFL_SHA = "5b9321a4298cfeb6b34354164a1c3afc3db114569984c502b9b35d988fd58c57"
WINDOWS_SYSTEM = set("""
advapi32 bcrypt bcryptprimitives cfgmgr32 combase comctl32 coremessaging crypt32 d2d1 d3d11
dcomp dwmapi dwrite dxgi gdi32 imm32 iphlpapi kernel32 ktmw32 mfplat mfreadwrite msi msvcrt ncrypt
ntdll ole32 oleaut32 opengl32 powrprof psapi rpcrt4 secur32 setupapi shell32 shlwapi
synchronization ucrtbase uiautomationcore user32 userenv version windowscodecs winhttp
winmm ws2_32 wtsapi32 xaudio2_8
""".split())
WINDOWS_REDIST = {"vcruntime140.dll", "vcruntime140_1.dll", "msvcp140.dll"}
LINUX_BASE = {"ld-linux-x86-64.so.2", "libc.so.6", "libgcc_s.so.1", "libm.so.6",
              "libdl.so.2", "libpthread.so.0", "librt.so.1", "libutil.so.1"}
LINUX_UI = {
    "libharfbuzz.so.0", "libfontconfig.so.1", "libpangoft2-1.0.so.0",
    "libpangocairo-1.0.so.0", "libpango-1.0.so.0", "libgobject-2.0.so.0",
    "libglib-2.0.so.0", "libcairo.so.2", "libgstaudio-1.0.so.0",
    "libgstbase-1.0.so.0", "libgstreamer-1.0.so.0", "libgstapp-1.0.so.0",
    "libwebkitgtk-6.0.so.4", "libgtk-4.so.1", "libgraphene-1.0.so.0",
    "libsoup-3.0.so.0", "libgio-2.0.so.0", "libjavascriptcoregtk-6.0.so.1",
}
RENDERER_SOURCE = Path(__file__).resolve().parents[1] / "apps/desktop"
APP_ID = "dev.metro.desktop"
LINUX_ICONS = {
    f"icons/{APP_ID}.png": "metro-256.png",
    f"icons/{APP_ID}.svg": "metro.svg",
}
WINDOWS_ICON = "ui/nsh/icon.ico"
WINDOWS_EXECUTABLES = {"Metro.exe", "ui/Metro.exe", "metro-server.exe", TUI_NAMES["windows"]}
# Production modules in apps/desktop/src/ui that import bundled SVG icons. Only
# these approve renderer assets: a test or fixture module never does.
ICON_MODULES = ("chrome.tsx", "activity.tsx", "chatControls.tsx", "library.tsx", "messageDelete.tsx")
BRIDGE_ENV = {"METRO_DESKTOP_BRIDGE_URL", "METRO_DESKTOP_BRIDGE_TOKEN"}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def regular(path):
    require(not path.is_symlink() and path.is_file() and path.stat().st_size > 0,
            f"missing, empty, or not a regular release file: {path}")
    return path


def directory(path):
    junction = getattr(path, "is_junction", lambda: False)
    require(not path.is_symlink() and not junction() and path.is_dir(),
            f"missing or not a normal release directory: {path}")
    return path


def read_json(path):
    value = json.loads(regular(path).read_text(encoding="utf-8"))
    require(isinstance(value, dict), f"{path}: expected a JSON object")
    return value


def sha256(path):
    with regular(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def copy(source, dest, executable=False):
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(regular(source), dest)
    dest.chmod(0o755 if executable else 0o644)


def write(path, text):
    path.write_bytes(text.encode("utf-8"))
    path.chmod(0o644)


def ico_images(path):
    """Return the ICO directory metadata and the bytes of each image."""
    ico = regular(path).read_bytes()
    reserved, kind, count = struct.unpack_from("<3H", ico)
    end = 6 + 16 * count
    require(reserved == 0 and kind == 1 and count > 1 and end <= len(ico),
            f"{path}: expected a multi-resolution ICO")
    images = []
    for i in range(6, end, 16):
        length, start = struct.unpack_from("<II", ico, i + 8)
        require(length > 0 and end <= start <= start + length <= len(ico),
                f"{path}: invalid ICO image")
        images.append((ico[i:i + 12], ico[start:start + length]))
    require(len({meta[:2] for meta, _ in images}) > 1,
            f"{path}: ICO images must have different sizes")
    return images


def pe_icon(resources, resource_rva, path, images):
    """Read numeric PE resource types/names/languages and verify icon group 1."""
    def invalid(message):
        return f"{path}: {message}"

    def unpack(fmt, relative):
        require(0 <= relative <= len(resources) - struct.calcsize(fmt),
                invalid("invalid PE resource directory offset"))
        return struct.unpack_from(fmt, resources, relative)

    def find(relative, id, directory):
        named, numbered = unpack("<HH", relative + 12)
        start = relative + 16 + 8 * named
        for i in range(numbered):
            name, target = unpack("<II", start + 8 * i)
            if id is not None and name != id:
                continue
            require(bool(target & 0x80000000) == directory, invalid("invalid PE icon resource tree"))
            return target & 0x7fffffff, name
        raise ValueError(invalid(f"missing PE icon resource (id {id})"))

    def payload(directory, lang):
        leaf, language = find(directory, lang, False)
        rva, size = unpack("<II", leaf)
        require(resource_rva <= rva and rva + size <= resource_rva + len(resources),
                invalid("invalid PE icon resource bounds"))
        start = rva - resource_rva
        return resources[start:start + size], language

    group_type, _ = find(0, 14, True)  # RT_GROUP_ICON
    icon_type, _ = find(0, 3, True)   # RT_ICON
    group, _ = find(group_type, 1, True)
    contents, lang = payload(group, None)
    require(len(contents) == 6 + 14 * len(images)
            and struct.unpack_from("<3H", contents) == (0, 1, len(images)),
            invalid("PE GROUP_ICON 1 must contain all Metro icon sizes"))
    ids = set()
    for i, (meta, expected) in enumerate(images):
        start = 6 + 14 * i
        (id,) = struct.unpack_from("<H", contents, start + 12)
        require(contents[start:start + 12] == meta and id not in ids,
                invalid("PE GROUP_ICON 1 does not match the Metro ICO"))
        ids.add(id)
        entry, _ = find(icon_type, id, True)
        actual, _ = payload(entry, lang)
        require(actual == expected, invalid(f"PE RT_ICON {id} does not match the Metro ICO"))


def pe(path, gui=False, icon=None):
    """Inspect PE32+ header, imports and optional icon without host binutils."""
    data = regular(path).read_bytes()

    def u16(offset):
        return struct.unpack_from("<H", data, offset)[0]

    def u32(offset):
        return struct.unpack_from("<I", data, offset)[0]

    require(data.startswith(b"MZ"), f"{path}: expected PE executable")
    header = u32(0x3c)
    opt = header + 24
    size = u16(header + 20)
    require(data[header:header + 4] == b"PE\0\0" and u16(header + 4) == 0x8664
            and u16(opt) == 0x20B and size >= 128 and not (u16(header + 22) & 0x2000),
            f"{path}: expected x64 PE32+ executable, not a DLL")
    subsystem = u16(opt + 68)
    require(subsystem == 2 if gui else subsystem in (2, 3),
            f"{path}: unexpected Windows subsystem {subsystem}")
    sections = [
        struct.unpack_from("<IIII", data, opt + size + 40 * i + 8)
        for i in range(u16(header + 6))
    ]

    def offset(rva, size=1):
        for _, start, raw_size, raw_offset in sections:
            if start <= rva < start + raw_size and rva + size <= start + raw_size:
                return raw_offset + rva - start
        raise ValueError(f"{path}: invalid PE section address")

    imports = set()
    count = min(u32(opt + 108), (size - 112) // 8)
    # IMAGE_IMPORT_DESCRIPTOR and IMAGE_DELAYLOAD_DESCRIPTOR.
    for index, stride, name_field in ((1, 20, 12), (13, 32, 4)):
        if count <= index:
            continue
        rva, length = struct.unpack_from("<II", data, opt + 112 + 8 * index)
        if not rva:
            continue
        for i in range(length // stride):
            pos = offset(rva + i * stride)
            if not any(data[pos:pos + stride]):
                break
            require(index != 13 or u32(pos) == 1, f"{path}: unsupported PE delay-load descriptor")
            start = offset(u32(pos + name_field))
            end = data.find(b"\0", start, start + 256)
            require(end != -1, f"{path}: invalid PE import name")
            imports.add(data[start:end].decode("ascii").lower())
        else:
            raise ValueError(f"{path}: unterminated PE import table")
    unknown = imports - WINDOWS_REDIST - {f"{name}.dll" for name in WINDOWS_SYSTEM}
    unknown = {name for name in unknown if not name.startswith(("api-ms-win-", "ext-ms-win-"))}
    require(not unknown, f"{path}: unhandled DLL dependencies: {', '.join(sorted(unknown))}")
    require("kernel32.dll" in imports, f"{path}: no standard Windows import table found")
    if icon is not None:
        require(count > 2, f"{path}: missing PE resource directory")
        rva, length = struct.unpack_from("<II", data, opt + 112 + 8 * 2)
        require(rva != 0 and length >= 16, f"{path}: missing PE resource directory")
        start = offset(rva, length)
        require(start + length <= len(data), f"{path}: truncated PE resource directory")
        pe_icon(data[start:start + length], rva, path, ico_images(icon))
    return sorted(imports)


def elf(path, max_glibc, ui=False):
    with regular(path).open("rb") as stream:
        header = stream.read(64)
    require(header[:6] == b"\x7fELF\x02\x01" and header[18:20] == b"\x3e\0"
            and header[16:18] in (b"\x02\0", b"\x03\0"), f"{path}: expected Linux x64 ELF")
    output = subprocess.check_output(
        ["readelf", "-l", "-d", "-V", str(path)], text=True, env={**os.environ, "LC_ALL": "C"}
    )
    require("Requesting program interpreter: /lib64/ld-linux-x86-64.so.2" in output
            and not re.search(r"\((?:RUNPATH|RPATH)\)", output),
            f"{path}: unexpected ELF interpreter or embedded library search path")
    glibc = max((tuple(map(int, v.split(".")))
                 for v in re.findall(r"Name: GLIBC_(\d+(?:\.\d+)+)", output)), default=())
    require(bool(glibc) and glibc <= max_glibc,
            f"{path}: needs glibc {glibc}, expected at most {max_glibc}")
    needed = set(re.findall(r"Shared library: \[([^\]]+)\]", output))
    unknown = needed - LINUX_BASE - (LINUX_UI if ui else set())
    require(not unknown, f"{path}: unhandled shared libraries: {', '.join(sorted(unknown))}")
    return {"glibc": ".".join(map(str, glibc)), "needed": sorted(needed)}


def nsh_assets(actual, source):
    """Allow only the imported production icons and the pinned Inter font."""
    names = set()
    for module in ICON_MODULES:
        text = regular(source / "src/ui" / module).read_text(encoding="utf-8")
        imports = re.findall(r"""^import [^;\n]+ from ["']\./icons/([a-z0-9-]+\.svg)["'];""", text, re.M)
        require(bool(imports) and len(imports) == len(set(imports)),
                f"production src/ui/{module} must import distinct SVG icons from src/ui/icons")
        names.update(imports)
    expected = {Path("src/ui/icons") / name for name in names} | {INTER_FONT}
    parents = {parent for p in expected for parent in p.parents}
    found = {}

    def visit(here):
        for path in sorted(directory(here).iterdir()):
            relative = path.relative_to(actual)
            if relative in parents:
                visit(path)
            else:
                require(relative in expected, f"unapproved renderer asset: {relative}")
                source_bytes = regular(source / relative).read_bytes()
                bundled_bytes = regular(path).read_bytes()
                # CI builds the NSH bundle on Linux; a Windows checkout can
                # have CRLF in the same tracked SVG text.
                if relative == INTER_FONT:
                    require(hashlib.sha256(source_bytes).hexdigest() == INTER_FONT_SHA
                            and hashlib.sha256(bundled_bytes).hexdigest() == INTER_FONT_SHA,
                            f"unexpected Inter font digest: {relative}")
                    matches = bundled_bytes == source_bytes
                else:
                    matches = bundled_bytes == source_bytes or (
                        os.name == "nt" and bundled_bytes == source_bytes.replace(b"\r\n", b"\n")
                    )
                require(not path.stat().st_mode & 0o111 and matches,
                        f"renderer asset must be an exact non-executable copy of its source: {relative}")
                found[relative] = path

    visit(actual)
    require(found.keys() == expected,
            f"missing renderer assets: {', '.join(map(str, sorted(expected - found.keys())))}")
    return found


def nsh_resources(ui_dir, source, version, app_id=APP_ID):
    resources = directory(directory(ui_dir) / "nsh")
    require({p.name for p in resources.iterdir()} == {"app.json", "bundle.nshc", "capabilities.json", "assets"},
            "expected NSH resources (app.json, bundle.nshc, capabilities.json, assets) and no extras")
    app = read_json(resources / "app.json")
    require((app.get("name"), app.get("id"), app.get("version")) == ("Metro", app_id, version),
            "NSH manifest name, ID or version does not match; set --app-version when running nsh package")
    require(not any(app.get(key) for key in ("singleInstance", "schemes", "fileTypes")),
            "the stock renderer must not register itself as the top-level launcher")
    declared = read_json(source / "nsh.json").get("capabilities")
    require(isinstance(declared, dict) and isinstance(declared.get("network"), dict)
            and isinstance(declared.get("process"), dict)
            and BRIDGE_ENV <= set(declared["process"].get("env", [])),
            "production NSH source manifest must declare the launcher bridge environment capabilities")
    # NSH 0.6.1 writes these explicit deny-by-default values when not set.
    expected = {
        **declared,
        "network": {"http": [], **declared["network"]},
        "process": {"execute": [], "exit": False, **declared["process"]},
    }
    capabilities = read_json(resources / "capabilities.json")
    require(json.dumps(capabilities, sort_keys=True) == json.dumps(expected, sort_keys=True),
            "packaged capabilities differ from the production NSH source manifest")
    assets = nsh_assets(resources / "assets", source)
    return {
        **{Path(name): regular(resources / name) for name in ("app.json", "bundle.nshc", "capabilities.json")},
        **{Path("assets") / relative: path for relative, path in assets.items()},
    }


def brand_notice(source, resources):
    notice = regular(source / "src/ui/icons/LICENSE-brand-marks.txt")
    text = notice.read_text(encoding="utf-8")
    shipped = {path.name for path in resources if path.name.startswith("brand-")}
    recorded = set(re.findall(r"^\s+(brand-[a-z0-9-]+\.svg)\s+<- ", text, re.M))
    require(bool(shipped) and shipped == recorded,
            "packaged provider marks must match the brand notice's attribution list")
    require("@lobehub/icons-static-svg 1.105.0 (MIT)" in text
            and "MIT License" in text and "Copyright (c) 2023 LobeHub" in text,
            "expected the pinned LobeHub provider marks attribution and license")
    return notice


def app_icon(name):
    return regular(RENDERER_SOURCE / "assets" / name)


def write_inter_notice(stage):
    source = regular(RENDERER_SOURCE / "src/ui/fonts/OFL.txt")
    contents = source.read_bytes()
    if os.name == "nt":
        contents = contents.replace(b"\r\n", b"\n")
    require(hashlib.sha256(contents).hexdigest() == INTER_OFL_SHA, f"{source}: unexpected Inter license")
    dest = stage / "licenses/INTER-OFL.txt"
    dest.parent.mkdir(parents=True, exist_ok=True)
    dest.write_bytes(contents)
    dest.chmod(0o644)


def linux_icon_assets():
    assets = {dest: app_icon(name) for dest, name in LINUX_ICONS.items()}
    for path in assets.values():
        if path.suffix == ".png":
            with path.open("rb") as stream:
                header = stream.read(24)
            require(header == b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR" + struct.pack(">II", 256, 256),
                    f"{path}: expected a 256×256 PNG")
        else:
            require(ET.parse(path).getroot().tag == "{http://www.w3.org/2000/svg}svg",
                    f"{path}: expected an SVG")
    return assets


def verify_desktop_icons(root, windows):
    """Check extracted archives against the same source icons as the build."""
    if windows:
        ico = app_icon("metro.ico")
        require(sha256(root / WINDOWS_ICON) == sha256(ico), "packaged NSH ICO differs from Metro's source")
        pe(root / "Metro.exe", gui=True, icon=ico)
    else:
        for dest, path in linux_icon_assets().items():
            require(sha256(root / dest) == sha256(path), f"packaged icon differs from source: {dest}")
        script = regular(root / "install-desktop")
        require(script.stat().st_mode & 0o111
                and sha256(script) == sha256(Path(__file__).with_name("install-desktop.sh")),
                "expected the executable freedesktop integration script")


def desktop(args, stage):
    platform = args.target.split("-")[0]
    windows = platform == "windows"
    executable = "Metro.exe" if windows else "Metro"
    renderer = args.ui_dir / executable
    resources = nsh_resources(args.ui_dir, RENDERER_SOURCE, args.version.removeprefix("v"))
    upstream_path, expected = NSH_APPS[platform]
    require(sha256(args.nsh_root / "runtime" / platform / upstream_path) == expected
            and sha256(renderer) == expected, f"renderer must be the stock NSH {NSH_VERSION} {platform} app")
    license_path = args.nsh_root / "LICENSE-APACHE"
    require(sha256(license_path) == NSH_LICENSE_SHA, "unexpected NSH license text")
    tui = [args.tui] if args.tui else []
    require(len({sha256(args.launcher), sha256(args.server), expected, *map(sha256, tui)}) == 3 + len(tui),
            "launcher, server, terminal client and NSH renderer must be separate executables")
    if windows:
        ico = app_icon("metro.ico")
        deps = {executable: pe(args.launcher, gui=True, icon=ico), f"ui/{executable}": pe(renderer, gui=True)}
        # The stock Windows runner loads this external icon for its windows.
        # Do not let NSH's native Windows packager rewrite the pinned EXE.
        copy(ico, stage / WINDOWS_ICON)
        integration = ""
        prerequisites = (
            "Windows 11 x64 with Windows Media Foundation (install Media Feature Pack on N editions).\n"
            "Install Microsoft's current Visual C++ v14 Redistributable (x64) if needed:\n"
            "https://aka.ms/vs/17/release/vc_redist.x64.exe\n"
        )
    else:
        deps = {executable: elf(args.launcher, (2, 39)),
                f"ui/{executable}": elf(renderer, (2, 39), ui=True)}
        for dest, source in linux_icon_assets().items():
            copy(source, stage / dest)
        copy(Path(__file__).with_name("install-desktop.sh"), stage / "install-desktop", True)
        integration = (
            "\nOptional Linux applications-menu entry (no sudo): from this folder, run\n"
            "  ./install-desktop --install\n"
            "Metro will appear with its icon in compatible desktop environments.\n"
            "If you move this folder, run the command again from its new location.\n"
            "Run ./install-desktop --uninstall before deleting the folder to remove\n"
            "the menu entry and icons. This does not remove your Metro data.\n"
        )
        prerequisites = (
            "Linux x64 with glibc 2.39 or newer (e.g. Ubuntu 24.04 or Debian 13).\n"
            "Requires GTK 4, WebKitGTK 6.0 and GStreamer runtime libraries. Debian/Ubuntu:\n"
            "  sudo apt install libgtk-4-1 libwebkitgtk-6.0-4 gstreamer1.0-plugins-base\n"
        )
    copy(args.launcher, stage / executable, True)
    copy(renderer, stage / "ui" / executable, True)
    for relative, resource in resources.items():
        copy(resource, stage / "ui/nsh" / relative)
    copy(license_path, stage / "licenses/NSH-LICENSE-APACHE.txt")
    copy(brand_notice(RENDERER_SOURCE, resources), stage / "licenses/BRAND-MARKS.txt")
    write_inter_notice(stage)
    server_name = "metro-server.exe" if windows else "metro-server"
    deps[server_name] = pe(args.server) if windows else elf(args.server, (2, 39))
    copy(args.server, stage / server_name, True)
    if args.tui:
        # The terminal client ships in the same archive under a fixed name. Readers find it in
        # "files"; RELEASE.json gets no new key (winsign.py checks an exact key set).
        tui_name = TUI_NAMES[platform]
        deps[tui_name] = pe(args.tui) if windows else elf(args.tui, (2, 39))
        copy(args.tui, stage / tui_name, True)
        integration += f"\nTerminal client: run {tui_name} from a terminal in this folder.\n"
    write(stage / "START.txt", f"Extract this entire archive. Launch {executable} at the top level.\n"
          "Keep the bundled server and ui directory next to the launcher.\n\n"
          "Metro opens your private on-device workspace by default. To connect to\n"
          "a shared Metro server, open the account menu and choose Add a server.\n"
          "To change what opens at launch, use Startup server in the account menu\n"
          "to choose On this computer or a saved server.\n"
          + integration + "\n"
          + prerequisites)
    # The launcher reads the installer's enable-local-runner marker (see
    # desktop_tools::runner). Archives without this field predate that.
    return {"nsh": NSH_VERSION, "dependencies": deps, "connector": "linked", "local_runner": LOCAL_RUNNER}


def macho(path, cputype):
    """A thin 64-bit Mach-O executable for one architecture, linking only system libraries."""
    with regular(path).open("rb") as stream:
        header = stream.read(16)
    require(len(header) == 16 and header[:4] == b"\xcf\xfa\xed\xfe"
            and struct.unpack_from("<I", header, 4)[0] == cputype
            and struct.unpack_from("<I", header, 12)[0] == 2,  # MH_EXECUTE
            f"{path}: expected a thin 64-bit Mach-O executable for cputype {cputype:#x}")
    output = subprocess.check_output(["otool", "-L", str(path)], text=True)
    libs = [line.split(" (", 1)[0].strip() for line in output.splitlines()[1:] if line.strip()]
    unknown = [lib for lib in libs if not lib.startswith(("/usr/lib/", "/System/Library/"))]
    require(not unknown, f"{path}: links non-system libraries: {', '.join(unknown)}")
    return {"needed": sorted(libs)}


def codesign(*args):
    subprocess.run(["codesign", *args], check=True, capture_output=True, text=True)


def stock_renderer(inner_exe, stock, contents, target):
    """The inner executable is the pinned stock runtime, re-signed ad hoc for MetroUI.app.

    Trust comes from the pinned stock bytes only (macos_macho.py): identical
    outside the code signature, and a bundle seal that covers exactly that code,
    this Info.plist and this CodeResources under the MetroUI identifier."""
    helper = runpy.run_path(str(Path(__file__).with_name("macos_macho.py")))
    packaged = regular(inner_exe).read_bytes()
    try:
        helper["same_code"](packaged, regular(stock).read_bytes())
        helper["verify_adhoc_signature"](packaged, MACOS_UI_ID, regular(contents / "Info.plist").read_bytes(),
                                         regular(contents / "_CodeSignature/CodeResources").read_bytes())
    except helper["MachOError"] as exc:
        raise ValueError(f"inner executable must be the stock NSH {NSH_VERSION} {target} runtime "
                         f"(package with NSH_RUNTIME set to the pinned runtime): {exc}") from None


def macos(args, stage):
    """Outer Metro.app (launcher + server) around the stock, signed NSH MetroUI.app."""
    import plistlib
    require(sys.platform == "darwin", "macOS packages are assembled on macOS (codesign is required)")
    stock_sha, cputype = NSH_MACOS_APPS[args.target]
    version = args.version.removeprefix("v")
    base_version = version.split("-")[0].split("+")[0]
    stock = args.nsh_root / "runtime/macos/nsh-macos-app"
    require(sha256(stock) == stock_sha, f"--nsh-root must be the official NSH {NSH_VERSION} {args.target} runtime")
    license_path = args.nsh_root / "LICENSE-APACHE"
    require(sha256(license_path) == NSH_LICENSE_SHA, "unexpected NSH license text")

    ui = directory(args.ui_dir)
    require(ui.name.endswith(".app"), "--ui-dir must be the Metro.app that `nsh package --target macos` wrote")
    contents = directory(ui / "Contents")
    info = plistlib.loads(regular(contents / "Info.plist").read_bytes())
    require((info.get("CFBundleExecutable"), info.get("CFBundleIdentifier"), info.get("CFBundleShortVersionString"))
            == ("Metro", MACOS_UI_ID, version) and info.get("LSMinimumSystemVersion") == MACOS_MIN,
            f"inner NSH Info.plist must have executable Metro, identifier {MACOS_UI_ID} and version {version}; "
            f"run nsh package with --id {MACOS_UI_ID} --app-version {version}")
    icon = info.get("CFBundleIconFile")
    if icon and not icon.endswith(".icns"):
        icon += ".icns"
    require({p.name for p in contents.iterdir()} <= {"Info.plist", "MacOS", "Resources", "_CodeSignature", "PkgInfo"}
            and {p.name for p in directory(contents / "MacOS").iterdir()} == {"Metro"}
            and {p.name for p in directory(contents / "Resources").iterdir()} <= {"nsh", *([icon] if icon else [])},
            "unexpected files in the NSH app bundle")
    require(not any(p.is_symlink() for p in ui.rglob("*")), "the NSH app bundle must not contain links")
    resources = nsh_resources(contents / "Resources", RENDERER_SOURCE, version, MACOS_UI_ID)
    inner_exe = contents / "MacOS/Metro"
    codesign("--verify", "--strict", str(ui))
    # Digests of the signed inner app as NSH produced it (after its ad hoc
    # codesign); the stock runtime digest above is the unsigned original.
    source_seal = sha256(contents / "_CodeSignature/CodeResources")
    source_exe = sha256(inner_exe)
    stock_renderer(inner_exe, stock, contents, args.target)
    deps = {"Metro.app/Contents/Helpers/MetroUI.app/Contents/MacOS/Metro": macho(inner_exe, cputype),
            "Metro.app/Contents/MacOS/Metro": macho(args.launcher, cputype),
            "Metro.app/Contents/MacOS/metro-server": macho(args.server, cputype)}
    if args.tui:
        deps["Metro.app/" + TUI_NAMES["macos"]] = macho(args.tui, cputype)
    tui = [args.tui] if args.tui else []
    require(len({sha256(args.launcher), sha256(args.server), sha256(inner_exe), *map(sha256, tui)}) == 3 + len(tui),
            "launcher, server, terminal client and NSH renderer must be separate executables")

    app = stage / "Metro.app"
    out_contents = app / "Contents"
    inner = out_contents / "Helpers/MetroUI.app"
    shutil.copytree(ui, inner, symlinks=False, copy_function=shutil.copy2)

    def inner_unchanged(when):
        require(sha256(inner / "Contents/_CodeSignature/CodeResources") == source_seal
                and sha256(inner / "Contents/MacOS/Metro") == source_exe,
                f"the inner NSH app changed {when}")
        codesign("--verify", "--strict", str(inner))
    inner_unchanged("while it was copied")
    copy(args.launcher, out_contents / "MacOS/Metro", True)
    copy(args.server, out_contents / "MacOS/metro-server", True)
    if args.tui:
        copy(args.tui, app / TUI_NAMES["macos"], True)
    outer_info = {
        "CFBundleDevelopmentRegion": "en", "CFBundleExecutable": "Metro", "CFBundleIdentifier": APP_ID,
        "CFBundleName": "Metro", "CFBundleDisplayName": "Metro", "CFBundlePackageType": "APPL",
        "CFBundleShortVersionString": base_version,
        "CFBundleVersion": str(read_json(RENDERER_SOURCE / "nsh.json").get("build", 1)),
        "CFBundleSupportedPlatforms": ["MacOSX"], "LSMinimumSystemVersion": MACOS_MIN,
        "NSHighResolutionCapable": True, "NSPrincipalClass": "NSApplication",
    }
    icon_helper = runpy.run_path(str(Path(__file__).with_name("macos_icon.py")))
    outer_info["CFBundleIconFile"] = icon_helper["write_outer_icon"](out_contents, app_icon("metro-256.png"))
    (out_contents / "Info.plist").write_bytes(plistlib.dumps(outer_info))
    (out_contents / "Info.plist").chmod(0o644)
    # Ad hoc signatures only (CI preview). Sign nested code first; never --deep,
    # which would re-sign the stock inner app.
    codesign("--force", "--sign", "-", "--timestamp=none", str(out_contents / "MacOS/metro-server"))
    if args.tui:
        codesign("--force", "--sign", "-", "--timestamp=none", str(app / TUI_NAMES["macos"]))
    codesign("--force", "--sign", "-", "--timestamp=none", str(app))
    codesign("--verify", "--deep", "--strict", str(app))
    inner_unchanged("when the outer app was signed")
    copy(license_path, stage / "licenses/NSH-LICENSE-APACHE.txt")
    copy(brand_notice(RENDERER_SOURCE, resources), stage / "licenses/BRAND-MARKS.txt")
    write_inter_notice(stage)
    write(stage / "START.txt",
          "Metro for macOS.\n"
          "This build is ad hoc signed. It is not Developer ID signed or notarized,\n"
          "so macOS may refuse to open it. Do not change Gatekeeper settings for it.\n\n"
          f"Requires macOS {MACOS_MIN} or newer on {'Apple silicon' if args.target == 'macos-arm64' else 'an Intel Mac'}.\n"
          "Install with install-metro-macos.sh, which places Metro.app in\n"
          "~/Applications/Metro Desktop/ and keeps your data in\n"
          "~/Library/Application Support/Metro/Desktop.\n")
    return {"nsh": NSH_VERSION, "dependencies": deps, "connector": "linked", "local_runner": LOCAL_RUNNER,
            "signing": "adhoc",
            "status": "preview-not-verified",
            "renderer": {"stock_unsigned_sha256": stock_sha, "packaged_signed_sha256": source_exe,
                         "code_resources_sha256": source_seal}}


def runner(args, stage):
    deps = elf(args.binary, (2, 36))
    copy(args.binary, stage / "metro-runner", True)
    copy(Path(__file__).with_name("run-runner.sh"), stage / "run-runner", True)
    write(stage / "START.txt", """Metro runner for Linux x64; no source checkout required.
Requires Linux kernel 5.6+ with openat2 available, glibc 2.36 or newer
(Debian 12 or equivalent), and the system libgcc_s runtime.
Use a trusted Metro HTTPS URL and an existing runner workspace.
Store the runner token from Metro in a regular, owner-only mode-600 file.

  METRO_SERVER_URL=https://metro.example.invalid \\
  METRO_WORKSPACE=/path/to/workspace \\
  METRO_RUNNER_TOKEN_FILE=/path/to/token-file \\
  ./run-runner

Use the workspace path on this same Linux host. The runner starts and polls
outbound. For a direct invocation, ./metro-runner reads METRO_RUNNER_TOKEN
instead of METRO_RUNNER_TOKEN_FILE. An org/personal runner can list/read by
default; check/MCP require operator-configured Docker sandboxing.
""")
    return {"dependencies": {"metro-runner": deps}}


def files(stage):
    return sorted(path for path in stage.rglob("*") if path.is_file())


def archive(stage, output, root, windows):
    inputs = {f"{root}/{p.relative_to(stage).as_posix()}": p for p in files(stage)}
    expected = {name: sha256(p) for name, p in inputs.items()}
    if windows:
        # Windows stat/chmod does not preserve POSIX modes (in particular
        # writable data appears as 0666 and writable EXEs as 0777).
        modes = {name: stat.S_IFREG | (0o755 if p.relative_to(stage).as_posix() in WINDOWS_EXECUTABLES else 0o644)
                 for name, p in inputs.items()}
        with zipfile.ZipFile(output, "w") as result:
            for name, path in inputs.items():
                info = zipfile.ZipInfo(name)
                info.create_system = 3
                info.compress_type = zipfile.ZIP_DEFLATED
                info.external_attr = modes[name] << 16
                with path.open("rb") as src, result.open(info, "w") as dest:
                    shutil.copyfileobj(src, dest)
        with zipfile.ZipFile(output) as result:
            require(result.namelist() == list(expected), "unexpected ZIP entries")
            for name, digest in expected.items():
                info = result.getinfo(name)
                require(info.create_system == 3 and info.external_attr == modes[name] << 16,
                        f"bad ZIP member mode: {name}")
                with result.open(name) as src:
                    require(hashlib.file_digest(src, "sha256").hexdigest() == digest, f"bad ZIP member: {name}")
    else:
        with output.open("wb") as stream, gzip.GzipFile(filename="", mode="wb", fileobj=stream, mtime=0) as gzip_stream:
            with tarfile.open(mode="w|", fileobj=gzip_stream, format=tarfile.USTAR_FORMAT) as result:
                for name, path in inputs.items():
                    info = tarfile.TarInfo(name)
                    info.mode = stat.S_IMODE(path.stat().st_mode)
                    info.size = path.stat().st_size
                    with path.open("rb") as src:
                        result.addfile(info, src)
        with tarfile.open(output) as result:
            require(result.getnames() == list(expected) and all(p.isfile() for p in result),
                    "unexpected TAR entries")
            for name, digest in expected.items():
                with result.extractfile(name) as src:
                    require(hashlib.file_digest(src, "sha256").hexdigest() == digest, f"bad TAR member: {name}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    kinds = parser.add_subparsers(dest="kind", required=True)
    d = kinds.add_parser("desktop")
    d.add_argument("--target", required=True, choices=("windows-x64", "linux-x64", "macos-arm64", "macos-x64"))
    d.add_argument("--launcher", required=True, type=Path)
    d.add_argument("--server", required=True, type=Path)
    d.add_argument("--ui-dir", required=True, type=Path)
    d.add_argument("--nsh-root", required=True, type=Path)
    d.add_argument("--tui", type=Path, help="metro-tui terminal client built for --target (shipped in the archive)")
    r = kinds.add_parser("runner")
    r.add_argument("--binary", required=True, type=Path)
    check = kinds.add_parser("verify-desktop-icons")
    check.add_argument("--target", required=True, choices=("windows-x64", "linux-x64"))
    check.add_argument("root", type=Path, help="extracted Metro desktop archive root")
    for sub in (d, r):
        sub.add_argument("--version", required=True)
        sub.add_argument("--out-dir", type=Path, default=Path("dist"))
        sub.add_argument("--notices", required=True, type=Path,
                         help="third-party notices for the native input binaries")
        sub.add_argument("--metro-license", type=Path, help="Metro license (if granted by the authors)")
    args = parser.parse_args()
    try:
        if args.kind == "verify-desktop-icons":
            verify_desktop_icons(args.root, args.target == "windows-x64")
            return
        require(re.fullmatch(r"v?\d+\.\d+\.\d+(?:[-+][A-Za-z0-9][A-Za-z0-9.-]{0,30})?", args.version),
                "version must be a file-safe semantic version (e.g. 1.2.3 or v1.2.3-rc.1)")
        windows = args.kind == "desktop" and args.target == "windows-x64"
        target = args.target if args.kind == "desktop" else "linux-x64"
        label = "Metro" if args.kind == "desktop" else "Metro-runner"
        basename = f"{label}-{args.version}-{target}"
        filename = basename + (".zip" if windows else ".tar.gz")
        args.out_dir.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix=".metro-package-", dir=args.out_dir) as work:
            stage = Path(work) / "stage"
            stage.mkdir()
            if args.kind == "runner":
                manifest = runner(args, stage)
            elif args.target.startswith("macos-"):
                manifest = macos(args, stage)
            else:
                manifest = desktop(args, stage)
            copy(args.notices, stage / "licenses/THIRD-PARTY-NOTICES.txt")
            if args.metro_license:
                copy(args.metro_license, stage / "licenses/METRO-LICENSE.txt")
            manifest.update(version=args.version, target=target, kind=args.kind, files={
                path.relative_to(stage).as_posix(): sha256(path) for path in files(stage)
            })
            write(stage / "RELEASE.json", json.dumps(manifest, indent=2, sort_keys=True) + "\n")
            result = Path(work) / filename
            archive(stage, result, basename, windows)
            sidecar = result.with_name(filename + ".sha256")
            write(sidecar, f"{sha256(result)}  {filename}\n")
            result.replace(args.out_dir / filename)
            sidecar.replace(args.out_dir / sidecar.name)
        print(args.out_dir / filename)
    except (OSError, ValueError, struct.error, ET.ParseError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"package: {error}\n")


if __name__ == "__main__":
    main()
