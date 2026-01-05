use std::path::{Path, PathBuf};

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let third_party = root.parent().unwrap().join("third_party");

    let ogg = third_party.join("libogg");
    let vorbis = third_party.join("libvorbis");
    let theora = third_party.join("libtheora");
    let theoraplay = third_party.join("theoraplay");

    build_theoraplay(&theoraplay, &ogg, &vorbis, &theora);
    build_theora(&theora, &ogg);
    build_vorbis(&vorbis, &ogg);
    build_ogg(&ogg);

    if !cfg!(target_os = "windows") {
        println!("cargo:rustc-link-lib=pthread");
        println!("cargo:rustc-link-lib=m");
    }
}

fn build_theoraplay(theoraplay: &Path, ogg: &Path, vorbis: &Path, theora: &Path) {
    let mut build = cc::Build::new();
    build
        .file(theoraplay.join("theoraplay.c"))
        .include(theoraplay)
        .include(theora.join("include"))
        .include(vorbis.join("include"))
        .include(ogg.join("include"))
        .warnings(false);
    build.compile("theoraplay");
}

fn build_theora(theora: &Path, ogg: &Path) {
    let mut build = cc::Build::new();
    for file in [
        "apiwrapper.c",
        "bitpack.c",
        "decapiwrapper.c",
        "decinfo.c",
        "decode.c",
        "dequant.c",
        "fragment.c",
        "huffdec.c",
        "idct.c",
        "info.c",
        "internal.c",
        "quant.c",
        "state.c",
    ] {
        build.file(theora.join("lib").join(file));
    }
    build
        .include(theora.join("include"))
        .include(theora.join("lib"))
        .include(ogg.join("include"))
        .warnings(false);
    build.compile("theora");
}

fn build_vorbis(vorbis: &Path, ogg: &Path) {
    let mut build = cc::Build::new();
    for file in [
        "mdct.c",
        "smallft.c",
        "block.c",
        "envelope.c",
        "window.c",
        "lsp.c",
        "lpc.c",
        "analysis.c",
        "synthesis.c",
        "psy.c",
        "info.c",
        "floor1.c",
        "floor0.c",
        "res0.c",
        "mapping0.c",
        "registry.c",
        "codebook.c",
        "sharedbook.c",
        "lookup.c",
        "bitrate.c",
    ] {
        build.file(vorbis.join("lib").join(file));
    }
    build
        .include(vorbis.join("include"))
        .include(vorbis.join("lib"))
        .include(ogg.join("include"))
        .warnings(false);
    build.compile("vorbis");
}

fn build_ogg(ogg: &Path) {
    let mut build = cc::Build::new();
    for file in ["bitwise.c", "framing.c"] {
        build.file(ogg.join("src").join(file));
    }
    build.include(ogg.join("include")).warnings(false);
    build.compile("ogg");
}
