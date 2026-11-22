#!/usr/bin/env python3
"""Assemble release archives from already-built, target-specific inputs."""

import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import struct
import subprocess
import tarfile
import tempfile
import zipfile


NSH_VERSION = "0.6.1"
NSH_APPS = {
    "windows": ("nsh-windows-app.exe", "3cde53d3a3d7d7bfc5eb30c670739116629034d3c3da89232c1ccbd90e01fd0f"),
    "linux": ("nsh-linux-app", "0a382b9e76ae45b5a28afc7d9ba0ea169e8390c8a21b94e74874907907d6d517"),
}
NSH_LICENSE_SHA = "ec75427ad955e3c740f22e7dc3f6db6fc5b2cc1d9135c2af91cece8dd47a2d38"
WINDOWS_SYSTEM = set("""
advapi32 bcrypt bcryptprimitives cfgmgr32 combase comctl32 coremessaging crypt32 d2d1 d3d11
dcomp dwmapi dwrite dxgi gdi32 imm32 iphlpapi kernel32 ktmw32 mfplat mfreadwrite msvcrt ncrypt
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


def pe(path, gui=False):
    """Inspect PE32+ header and imports without depending on host binutils."""
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

    def offset(rva):
        for _, start, raw_size, raw_offset in sections:
            if start <= rva < start + raw_size:
                return raw_offset + rva - start
        raise ValueError(f"{path}: invalid PE import address")

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
    """Allow only exact copies of production chrome's imported SVG icons."""
    chrome = regular(source / "src/ui/chrome.tsx").read_text(encoding="utf-8")
    imports = re.findall(r"""^import [^;\n]+ from ["']\./icons/([a-z0-9-]+\.svg)["'];""", chrome, re.M)
    require(bool(imports) and len(imports) == len(set(imports)),
            "production chrome must import distinct SVG icons from src/ui/icons")
    expected = {Path("src/ui/icons") / name for name in imports}
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


def nsh_resources(ui_dir, source, version):
    resources = directory(directory(ui_dir) / "nsh")
    require({p.name for p in resources.iterdir()} == {"app.json", "bundle.nshc", "capabilities.json", "assets"},
            "expected NSH resources (app.json, bundle.nshc, capabilities.json, assets) and no extras")
    app = read_json(resources / "app.json")
    require((app.get("name"), app.get("id"), app.get("version")) == ("Metro", "dev.metro.desktop", version),
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
    require(len({sha256(args.launcher), sha256(args.server), expected}) == 3,
            "launcher, server and NSH renderer must be separate executables")
    if windows:
        deps = {executable: pe(args.launcher, gui=True), f"ui/{executable}": pe(renderer, gui=True)}
        prerequisites = (
            "Windows 11 x64 with Windows Media Foundation (install Media Feature Pack on N editions).\n"
            "Install Microsoft's current Visual C++ v14 Redistributable (x64) if needed:\n"
            "https://aka.ms/vs/17/release/vc_redist.x64.exe\n"
        )
    else:
        deps = {executable: elf(args.launcher, (2, 39)),
                f"ui/{executable}": elf(renderer, (2, 39), ui=True)}
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
    server_name = "metro-server.exe" if windows else "metro-server"
    deps[server_name] = pe(args.server) if windows else elf(args.server, (2, 39))
    copy(args.server, stage / server_name, True)
    write(stage / "START.txt", f"Extract this entire archive. Launch {executable} at the top level.\n"
          "Choose Open local workspace for a solo workspace, or enter your Metro service URL to sign in.\n"
          "Keep the bundled server and ui directory next to the launcher.\n\n"
          + prerequisites)
    return {"nsh": NSH_VERSION, "dependencies": deps, "connector": "linked"}


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
        with zipfile.ZipFile(output, "w") as result:
            for name, path in inputs.items():
                info = zipfile.ZipInfo(name)
                info.create_system = 3
                info.compress_type = zipfile.ZIP_DEFLATED
                info.external_attr = (stat.S_IFREG | stat.S_IMODE(path.stat().st_mode)) << 16
                with path.open("rb") as src, result.open(info, "w") as dest:
                    shutil.copyfileobj(src, dest)
        with zipfile.ZipFile(output) as result:
            require(result.namelist() == list(expected), "unexpected ZIP entries")
            for name, digest in expected.items():
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
    d.add_argument("--target", required=True, choices=("windows-x64", "linux-x64"))
    d.add_argument("--launcher", required=True, type=Path)
    d.add_argument("--server", required=True, type=Path)
    d.add_argument("--ui-dir", required=True, type=Path)
    d.add_argument("--nsh-root", required=True, type=Path)
    r = kinds.add_parser("runner")
    r.add_argument("--binary", required=True, type=Path)
    for sub in (d, r):
        sub.add_argument("--version", required=True)
        sub.add_argument("--out-dir", type=Path, default=Path("dist"))
        sub.add_argument("--notices", required=True, type=Path,
                         help="third-party notices for the native input binaries")
        sub.add_argument("--metro-license", type=Path, help="Metro license (if granted by the authors)")
    args = parser.parse_args()
    try:
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
            manifest = (desktop if args.kind == "desktop" else runner)(args, stage)
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
    except (OSError, ValueError, struct.error, subprocess.CalledProcessError) as error:
        parser.exit(1, f"package: {error}\n")


if __name__ == "__main__":
    main()
