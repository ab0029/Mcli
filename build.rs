use std::env;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

fn compile_icon(compiler: &mut Command, directory: &Path) -> io::Result<()> {
    let status = compiler.current_dir(directory).status()?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "resource compiler exited with {status}"
        )));
    }
    Ok(())
}

fn main() {
    println!("cargo:rerun-if-changed=assets/brand/mcli.rc");
    println!("cargo:rerun-if-changed=assets/brand/mcli.ico");
    println!("cargo:rerun-if-env-changed=RC");
    println!("cargo:rerun-if-env-changed=WINDRES");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let directory = Path::new("assets/brand");
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let target = env::var("TARGET").unwrap();
    let output = if target.ends_with("msvc") {
        let file = out.join("mcli-icon.res");
        let tool = env::var_os("RC")
            .map(PathBuf::from)
            .or_else(|| {
                let sdk = find_msvc_tools::find_windows_sdk(&target)?;
                sdk.path()
                    .map(|dir| dir.join("rc.exe"))
                    .find(|rc| rc.is_file())
            })
            .unwrap_or_else(|| "rc.exe".into());
        compile_icon(
            Command::new(tool)
                .arg("/nologo")
                .arg("/fo")
                .arg(&file)
                .arg("mcli.rc"),
            directory,
        )
        .expect("compile Mcli icon with the Windows SDK resource compiler (rc.exe)");
        file
    } else {
        let file = out.join("mcli-icon.o");
        let arch = target.split('-').next().unwrap();
        let coff = match arch {
            "x86_64" => "pe-x86-64",
            "i686" => "pe-i386",
            "aarch64" => "pe-aarch64-little",
            _ => panic!("unsupported Windows GNU resource target: {target}"),
        };
        let run = |tool: &std::ffi::OsStr| {
            compile_icon(
                Command::new(tool)
                    .args(["-F", coff, "-J", "rc", "-O", "coff", "-i", "mcli.rc", "-o"])
                    .arg(&file),
                directory,
            )
        };
        let result = match env::var_os("WINDRES") {
            Some(tool) => run(&tool),
            None => run(format!("{arch}-w64-mingw32-windres").as_ref()).or_else(|e| {
                if e.kind() == io::ErrorKind::NotFound {
                    run("windres".as_ref())
                } else {
                    Err(e)
                }
            }),
        };
        result.expect("compile Mcli icon with MinGW-w64 windres");
        file
    };
    // The CLI hook helper is a separate executable with no application window.
    println!("cargo:rustc-link-arg-bin=mcli={}", output.display());
}
