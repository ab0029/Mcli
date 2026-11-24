//! Each HPCON must be resized and closed by the implementation that created it.
use super::{coord, hresult};
use std::io;
use std::os::windows::io::{AsRawHandle, OwnedHandle};
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::Console::{
    COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON, ResizePseudoConsole,
};

type Create = unsafe extern "system" fn(COORD, HANDLE, HANDLE, u32, *mut HPCON) -> i32;
type Resize = unsafe extern "system" fn(HPCON, COORD) -> i32;
type Close = unsafe extern "system" fn(HPCON);

struct Api {
    create: Create,
    resize: Resize,
    close: Close,
}

static SYSTEM: Api = Api {
    create: CreatePseudoConsole,
    resize: ResizePseudoConsole,
    close: ClosePseudoConsole,
};

pub struct Console {
    pub(super) handle: HPCON,
    api: &'static Api,
}

impl Console {
    pub(super) fn create(
        size: COORD,
        input: &OwnedHandle,
        output: &OwnedHandle,
    ) -> io::Result<Self> {
        let open = |api: &'static Api| {
            let mut handle = 0;
            hresult(unsafe {
                (api.create)(
                    size,
                    input.as_raw_handle(),
                    output.as_raw_handle(),
                    0,
                    &mut handle,
                )
            })?;
            Ok(Self { handle, api })
        };
        #[cfg(target_arch = "x86_64")]
        if let Some(package) = packaged::get()
            && let Ok(console) = open(&package.api)
        {
            return Ok(console);
        }
        open(&SYSTEM)
    }

    pub fn resize(&self, cols: u16, lines: u16) -> io::Result<()> {
        hresult(unsafe { (self.api.resize)(self.handle, coord(cols, lines)) })
    }
}

impl Drop for Console {
    fn drop(&mut self) {
        unsafe { (self.api.close)(self.handle) };
    }
}

#[cfg(target_arch = "x86_64")]
mod packaged {
    use super::{Api, Close, Create, Resize};
    use std::fs::{File, OpenOptions};
    use std::io::Read;
    use std::os::windows::fs::OpenOptionsExt;
    use std::path::Path;
    use std::ptr::{null, null_mut};
    use std::sync::OnceLock;
    use windows_sys::Win32::Foundation::{FreeLibrary, HMODULE};
    use windows_sys::Win32::Security::Cryptography::{BCRYPT_SHA256_ALG_HANDLE, BCryptHash};
    use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
    use windows_sys::Win32::System::LibraryLoader::{
        GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
    };

    // Microsoft.Windows.Console.ConPTY 1.25.261109001, win-x64.
    const DLL: &str = "e971856f89748dc3b458125e9afdd79b5e9574a7c937676116d3cbbd3b71f492";
    const HOST: &str = "b17e48b07ef153b420f0a6e178bacd31fb4c3390e21ae746586213fc0719a775";

    pub(super) struct Package {
        pub(super) api: Api,
        // The DLL locates OpenConsole.exe beside itself on every launch.
        _files: [File; 2],
    }

    pub(super) fn get() -> Option<&'static Package> {
        static PACKAGE: OnceLock<Option<Package>> = OnceLock::new();
        PACKAGE
            .get_or_init(|| {
                let exe = std::env::current_exe().ok()?;
                load(&exe.parent()?.join("conpty"))
            })
            .as_ref()
    }

    fn verified_file(path: &Path, size: u32, expected: &str) -> Option<File> {
        let mut file = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(path)
            .ok()?;
        if file.metadata().ok()?.len() != u64::from(size) {
            return None;
        }
        let mut data = vec![0; size as usize];
        file.read_exact(&mut data).ok()?;
        let mut digest = [0u8; 32];
        let status = unsafe {
            BCryptHash(
                BCRYPT_SHA256_ALG_HANDLE,
                null(),
                0,
                data.as_ptr(),
                size,
                digest.as_mut_ptr(),
                32,
            )
        };
        (status >= 0
            && digest
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
                == expected)
            .then_some(file)
    }

    fn load(directory: &Path) -> Option<Package> {
        let dll = directory.join("conpty.dll");
        let files = [
            verified_file(&dll, 119608, DLL)?,
            verified_file(&directory.join("OpenConsole.exe"), 1089336, HOST)?,
        ];
        let path = super::super::wide(dll.as_os_str()).ok()?;
        let module =
            unsafe { LoadLibraryExW(path.as_ptr(), null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32) };
        if module.is_null() {
            return None;
        }
        // Successful pointers live for the process lifetime in PACKAGE.
        if let Some(api) = exports(module) {
            Some(Package { api, _files: files })
        } else {
            unsafe { FreeLibrary(module) };
            None
        }
    }

    fn exports(module: HMODULE) -> Option<Api> {
        // These three exports have the same WINAPI signatures as the system functions.
        unsafe {
            let create = GetProcAddress(module, c"ConptyCreatePseudoConsole".as_ptr().cast())?;
            let resize = GetProcAddress(module, c"ConptyResizePseudoConsole".as_ptr().cast())?;
            let close = GetProcAddress(module, c"ConptyClosePseudoConsole".as_ptr().cast())?;
            Some(Api {
                create: std::mem::transmute::<unsafe extern "system" fn() -> isize, Create>(create),
                resize: std::mem::transmute::<unsafe extern "system" fn() -> isize, Resize>(resize),
                close: std::mem::transmute::<unsafe extern "system" fn() -> isize, Close>(close),
            })
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn only_the_pinned_pair_is_eligible() {
            let dir = crate::tests::scratch(&format!("conpty-package-{}", std::process::id()));
            assert!(load(&dir).is_none());
            let path = dir.join("conpty.dll");
            std::fs::write(&path, b"abc").unwrap();
            let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
            assert!(verified_file(&path, 3, abc).is_some());
            assert!(verified_file(&path, 3, DLL).is_none());
            assert!(verified_file(&path, 4, abc).is_none());
            std::fs::write(path, vec![0; 119608]).unwrap();
            assert!(load(&dir).is_none());
        }

        #[test]
        #[ignore = "requires the signed x64 package next to the test executable"]
        fn packaged_backend_creates_and_resizes_the_console() {
            use crate::pty::{Console, coord, pipe};
            let api = &get().expect("missing pinned ConPTY pair").api;
            let (input, _writer) = pipe().unwrap();
            let (_reader, output) = pipe().unwrap();
            let console = Console::create(coord(80, 25), &input, &output).unwrap();
            assert!(std::ptr::eq(console.api, api));
            console.resize(100, 32).unwrap();
        }
    }
}
