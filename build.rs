fn main() {
    #[cfg(feature = "windows-client")]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=assets/windows/sshi.rc");
        println!("cargo:rerun-if-changed=assets/windows/sshi.ico");
        println!("cargo:rerun-if-changed=assets/windows/sshi.manifest");
        let version = env!("CARGO_PKG_VERSION");
        let defines = [
            format!(
                "SSHI_VERSION_NUM={},{},{},0",
                env!("CARGO_PKG_VERSION_MAJOR"),
                env!("CARGO_PKG_VERSION_MINOR"),
                env!("CARGO_PKG_VERSION_PATCH")
            ),
            format!("SSHI_VERSION_STR=\"{version}\""),
        ];
        embed_resource::compile_for(
            "assets/windows/sshi.rc",
            ["sshi-windows"],
            embed_resource::ParamsMacrosAndIncludeDirs(defines, ["assets/windows"]),
        )
        .manifest_required()
        .expect("embed sshi Windows resources");
    }
}
