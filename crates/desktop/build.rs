use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const ICON: &str = "../../apps/desktop/assets/metro.ico";

fn main() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    println!("cargo:rerun-if-changed={ICON}");
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo supplies OUT_DIR"));
    let res = out.join("metro-icon.res");
    let ico = fs::read(ICON).expect("read the Metro app icon");
    fs::write(&res, icon_resource(&ico)).expect("write the Metro icon resource");
    let resource = match env::var("CARGO_CFG_TARGET_ENV").as_deref() {
        // link.exe reads standard .res files directly, so no SDK rc.exe lookup
        // or dependency on a Visual Studio developer prompt is needed.
        Ok("msvc") => res,
        // GNU ld reads a COFF object; windres converts without recompiling the icon.
        Ok("gnu") => {
            let arch = env::var("CARGO_CFG_TARGET_ARCH").expect("Cargo supplies the target arch");
            let prefix = if arch == "x86" { "i686" } else { &arch };
            let tool = format!("{prefix}-w64-mingw32-windres");
            println!("cargo:rerun-if-env-changed=METRO_WINDRES");
            let configured = env::var_os("METRO_WINDRES");
            let mut command = Command::new(configured.as_deref().unwrap_or(tool.as_ref()));
            if configured.is_none() && env::var("HOST") == env::var("TARGET") {
                command = Command::new("windres");
            }
            let obj = out.join("metro-icon.o");
            let status = command
                .args(["-J", "res", "-O", "coff", "-i"])
                .arg(&res)
                .arg("-o")
                .arg(&obj)
                .status()
                .unwrap_or_else(|err| {
                    panic!("start {command:?} (set METRO_WINDRES if needed): {err}")
                });
            assert!(status.success(), "windres could not convert the Metro icon");
            obj
        }
        other => panic!("unsupported Windows resource toolchain: {other:?}"),
    };
    println!(
        "cargo:rustc-link-arg-bin=metro-desktop={}",
        resource.display()
    );
}

/// Each ICO image becomes an RT_ICON (3); the group (14, name 1) has
/// the same directory entries but replaces the file offsets with resource IDs.
fn icon_resource(ico: &[u8]) -> Vec<u8> {
    assert_eq!(
        ico.get(..4),
        Some(&b"\0\0\x01\0"[..]),
        "expected an ICO file"
    );
    let count = u16::from_le_bytes(ico[4..6].try_into().unwrap());
    assert!(count > 1, "expected a multi-resolution Metro icon");
    let mut res = Vec::new();
    // A .res stream starts with a null type/name resource header.
    resource(&mut res, 0, 0, &[]);
    let mut group = ico[..6].to_vec();
    let directory_end = 6 + usize::from(count) * 16;
    let (entries, _) = ico[6..directory_end].as_chunks::<16>();
    for (index, entry) in (1..=count).zip(entries) {
        let len = u32::from_le_bytes(entry[8..12].try_into().unwrap()) as usize;
        let offset = u32::from_le_bytes(entry[12..16].try_into().unwrap()) as usize;
        assert!(len != 0 && offset >= directory_end, "invalid ICO image");
        resource(&mut res, 3, index, &ico[offset..][..len]);
        group.extend_from_slice(&entry[..12]);
        group.extend_from_slice(&index.to_le_bytes());
    }
    resource(&mut res, 14, 1, &group);
    res
}

/// Binary Windows resource format: a 32-byte ordinal type/name header,
/// followed by the payload padded to a 4-byte boundary.
fn resource(out: &mut Vec<u8>, kind: u16, id: u16, data: &[u8]) {
    out.extend_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
    out.extend_from_slice(&32_u32.to_le_bytes()); // header size
    for word in [0xffff, kind, 0xffff, id] {
        out.extend_from_slice(&word.to_le_bytes());
    }
    out.extend_from_slice(&[0; 16]); // data version, flags, neutral language, version, characteristics
    out.extend_from_slice(data);
    out.resize(out.len().next_multiple_of(4), 0);
}
