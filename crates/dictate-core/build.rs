fn main() {
    println!("cargo:rerun-if-changed=../../native/ffmpeg_bridge.c");
    println!("cargo:rerun-if-changed=../../native/ffmpeg_bridge.h");
    if std::env::var_os("CARGO_FEATURE_STATIC_LIBAV").is_some() {
        let libraries = ["libavformat", "libavcodec", "libswresample", "libavutil"];
        let mut include_paths = Vec::new();
        for library in libraries {
            let found = pkg_config::Config::new()
                .statik(true)
                .cargo_metadata(false)
                .probe(library)
                .unwrap_or_else(|_| panic!("{library} static library is required"));
            include_paths.extend(found.include_paths);
        }
        let mut build = cc::Build::new();
        build
            .file("../../native/ffmpeg_bridge.c")
            .includes(include_paths)
            .warnings(false)
            .compile("dictate_ffmpeg_bridge");
        for library in libraries {
            pkg_config::Config::new()
                .statik(true)
                .probe(library)
                .unwrap_or_else(|_| panic!("{library} static library is required"));
        }
    }

    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");
}
