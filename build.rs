use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=assets/tailout.rc");
    println!("cargo:rerun-if-changed=assets/tailout.ico");
    println!("cargo:rerun-if-env-changed=RC");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let target = env::var("TARGET").expect("Cargo target");
    let assets = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("assets");
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let msvc = target.contains("msvc");
    let resource = out.join(if msvc { "tailout.res" } else { "tailout.o" });
    let mut compiler = if let Some(compiler) = env::var_os("RC") {
        Command::new(compiler)
    } else if msvc {
        find_msvc_tools::find(&target, "rc.exe").unwrap_or_else(|| Command::new("llvm-rc"))
    } else {
        Command::new("windres")
    };
    compiler.current_dir(assets);
    if msvc {
        compiler
            .args(["/nologo", "/fo"])
            .arg(&resource)
            .arg("tailout.rc");
    } else {
        compiler
            .args([
                "--input",
                "tailout.rc",
                "--output-format",
                "coff",
                "--output",
            ])
            .arg(&resource);
        match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
            Ok("x86_64") => {
                compiler.args(["--target", "pe-x86-64"]);
            }
            Ok("x86") => {
                compiler.args(["--target", "pe-i386"]);
            }
            _ => {}
        }
    }
    let status = compiler.status().expect("Run the Windows resource compiler (Windows SDK rc.exe, llvm-rc, or windres; override with RC)");
    assert!(
        status.success(),
        "Compiling Tailout's Windows icon failed: {status}"
    );
    println!("cargo:rustc-link-arg-bin=tailout={}", resource.display());
}
