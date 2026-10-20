//! Embeds the icon, manifest and version info into the Windows exe.

use std::env;
use std::fs;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=assets/majihanaya.ico");
    println!("cargo:rerun-if-changed=assets/majihanaya.manifest");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let asset = |name: &str| Path::new(&dir).join("assets").join(name).display().to_string().replace('\\', "/");
    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let nums: Vec<u16> = version.split(['.', '-']).take(3).map(|p| p.parse().unwrap_or(0)).collect();
    let (major, minor, patch) = (nums[0], nums[1], nums[2]);

    let rc = format!(
        r#"#pragma code_page(65001)
#include <winres.h>

1 ICON "{icon}"
1 RT_MANIFEST "{manifest}"

VS_VERSION_INFO VERSIONINFO
 FILEVERSION {major},{minor},{patch},0
 PRODUCTVERSION {major},{minor},{patch},0
 FILEFLAGSMASK 0x3fL
 FILEFLAGS 0x0L
 FILEOS VOS_NT_WINDOWS32
 FILETYPE VFT_APP
 FILESUBTYPE 0x0L
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904b0"
        BEGIN
            VALUE "CompanyName", "majimawrks"
            VALUE "FileDescription", "majihanaya"
            VALUE "FileVersion", "{version}"
            VALUE "InternalName", "majihanaya"
            VALUE "LegalCopyright", "© 2026 majimawrks. GPL-3.0-or-later."
            VALUE "OriginalFilename", "majihanaya.exe"
            VALUE "ProductName", "majihanaya"
            VALUE "ProductVersion", "{version}"
            VALUE "Comments", "Tray widget showing Claude and Codex plan-limit usage. https://github.com/majimawrks/majihanaya"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x409, 1200
    END
END
"#,
        icon = asset("majihanaya.ico"),
        manifest = asset("majihanaya.manifest"),
    );

    let out = Path::new(&env::var("OUT_DIR").unwrap()).join("majihanaya.rc");
    fs::write(&out, rc).unwrap();
    embed_resource::compile(&out, embed_resource::NONE).manifest_required().unwrap();
}
