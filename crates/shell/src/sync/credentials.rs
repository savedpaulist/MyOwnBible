//! The Google refresh token is separate from the portable journal and exports.

#[cfg(target_os = "macos")]
mod os {
    use std::{ffi::c_void, ptr, slice};

    use anyhow::{Result, ensure};

    const SERVICE: &[u8] = b"BibleOWN Google Drive";
    const ITEM_NOT_FOUND: i32 = -25300;

    #[link(name = "Security", kind = "framework")]
    unsafe extern "C" {
        fn SecKeychainFindGenericPassword(
            keychain: *const c_void,
            service_len: u32,
            service: *const u8,
            account_len: u32,
            account: *const u8,
            password_len: *mut u32,
            password: *mut *mut c_void,
            item: *mut *mut c_void,
        ) -> i32;
        fn SecKeychainAddGenericPassword(
            keychain: *const c_void,
            service_len: u32,
            service: *const u8,
            account_len: u32,
            account: *const u8,
            password_len: u32,
            password: *const u8,
            item: *mut *mut c_void,
        ) -> i32;
        fn SecKeychainItemModifyAttributesAndData(
            item: *mut c_void,
            attrs: *const c_void,
            len: u32,
            data: *const u8,
        ) -> i32;
        fn SecKeychainItemFreeContent(attrs: *const c_void, data: *mut c_void) -> i32;
        fn SecKeychainItemDelete(item: *mut c_void) -> i32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(value: *mut c_void);
    }

    fn check(status: i32) -> Result<()> {
        ensure!(status == 0, "Связка ключей macOS вернула ошибку {status}");
        Ok(())
    }

    fn find(id: &str, len: *mut u32, data: *mut *mut c_void, item: *mut *mut c_void) -> i32 {
        // Security.framework takes lengths in bytes and allocates returned data.
        unsafe {
            SecKeychainFindGenericPassword(
                ptr::null(),
                SERVICE.len() as u32,
                SERVICE.as_ptr(),
                id.len() as u32,
                id.as_ptr(),
                len,
                data,
                item,
            )
        }
    }

    pub fn load(id: &str) -> Result<Option<String>> {
        let mut len = 0;
        let mut data = ptr::null_mut();
        let status = find(id, &mut len, &mut data, ptr::null_mut());
        if status == ITEM_NOT_FOUND {
            return Ok(None);
        }
        check(status)?;
        if data.is_null() {
            return Ok(None);
        }
        let bytes = unsafe {
            let bytes = slice::from_raw_parts(data.cast::<u8>(), len as usize).to_vec();
            SecKeychainItemFreeContent(ptr::null(), data);
            bytes
        };
        Ok(Some(String::from_utf8(bytes)?))
    }

    pub fn store(id: &str, token: &str) -> Result<()> {
        let mut item = ptr::null_mut();
        let status = find(id, ptr::null_mut(), ptr::null_mut(), &mut item);
        if status == ITEM_NOT_FOUND {
            return check(unsafe {
                SecKeychainAddGenericPassword(
                    ptr::null(),
                    SERVICE.len() as u32,
                    SERVICE.as_ptr(),
                    id.len() as u32,
                    id.as_ptr(),
                    token.len() as u32,
                    token.as_ptr(),
                    ptr::null_mut(),
                )
            });
        }
        check(status)?;
        check(unsafe {
            let status = SecKeychainItemModifyAttributesAndData(
                item,
                ptr::null(),
                token.len() as u32,
                token.as_ptr(),
            );
            CFRelease(item);
            status
        })
    }

    pub fn clear(id: &str) -> Result<()> {
        let mut item = ptr::null_mut();
        let status = find(id, ptr::null_mut(), ptr::null_mut(), &mut item);
        if status == ITEM_NOT_FOUND {
            return Ok(());
        }
        check(status)?;
        check(unsafe {
            let status = SecKeychainItemDelete(item);
            CFRelease(item);
            status
        })
    }
}

#[cfg(target_os = "linux")]
mod os {
    use std::{
        io::Write,
        process::{Command, Stdio},
    };

    use anyhow::{Context as _, Result, ensure};

    fn tool(action: &str, id: &str) -> Command {
        let mut command = Command::new("secret-tool");
        command.arg(action);
        if action == "store" {
            command.arg("--label=BibleOWN Google Drive");
        }
        command.args(["application", "BibleOWN", "library", id]);
        command
    }

    pub fn load(id: &str) -> Result<Option<String>> {
        let output = tool("lookup", id)
            .output()
            .context("Для входа в Google Drive установите secret-tool")?;
        if output.status.code() == Some(1) && output.stderr.is_empty() {
            return Ok(None);
        }
        ensure!(
            output.status.success(),
            "Хранилище секретов Linux недоступно"
        );
        Ok(Some(
            String::from_utf8(output.stdout)?.trim_end().to_owned(),
        ))
    }

    pub fn store(id: &str, token: &str) -> Result<()> {
        let mut child = tool("store", id)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("Для входа в Google Drive установите secret-tool")?;
        let result = child
            .stdin
            .take()
            .context("Хранилище секретов Linux недоступно")?
            .write_all(token.as_bytes());
        let status = child.wait()?;
        result?;
        ensure!(status.success(), "Хранилище секретов Linux недоступно");
        Ok(())
    }

    pub fn clear(id: &str) -> Result<()> {
        let output = tool("clear", id)
            .output()
            .context("Хранилище секретов Linux недоступно")?;
        ensure!(
            output.status.success()
                || (output.status.code() == Some(1) && output.stderr.is_empty()),
            "Не удалось очистить хранилище секретов Linux"
        );
        Ok(())
    }
}

#[cfg(target_os = "windows")]
mod os {
    use std::{ffi::c_void, ptr, slice};

    use anyhow::{Result, ensure};

    const GENERIC: u32 = 1;
    const PERSIST_LOCAL_MACHINE: u32 = 2;
    const NOT_FOUND: i32 = 1168;

    #[repr(C)]
    struct Credential {
        flags: u32,
        kind: u32,
        target_name: *const u16,
        comment: *const u16,
        last_written: [u32; 2],
        blob_size: u32,
        blob: *const u8,
        persist: u32,
        attribute_count: u32,
        attributes: *const c_void,
        target_alias: *const u16,
        user_name: *const u16,
    }

    #[link(name = "Advapi32")]
    unsafe extern "system" {
        fn CredReadW(target: *const u16, kind: u32, flags: u32, out: *mut *mut Credential) -> i32;
        fn CredWriteW(credential: *const Credential, flags: u32) -> i32;
        fn CredDeleteW(target: *const u16, kind: u32, flags: u32) -> i32;
        fn CredFree(buffer: *mut Credential);
    }

    fn target(id: &str) -> Vec<u16> {
        format!("BibleOWN Google Drive/{id}")
            .encode_utf16()
            .chain([0])
            .collect()
    }

    pub fn load(id: &str) -> Result<Option<String>> {
        let name = target(id);
        let mut credential = ptr::null_mut();
        if unsafe { CredReadW(name.as_ptr(), GENERIC, 0, &mut credential) } == 0 {
            let error = std::io::Error::last_os_error();
            return if error.raw_os_error() == Some(NOT_FOUND) {
                Ok(None)
            } else {
                Err(error.into())
            };
        }
        let bytes = unsafe {
            let bytes = if (*credential).blob_size > 0 {
                slice::from_raw_parts((*credential).blob, (*credential).blob_size as usize).to_vec()
            } else {
                Vec::new()
            };
            CredFree(credential);
            bytes
        };
        Ok(Some(String::from_utf8(bytes)?))
    }

    pub fn store(id: &str, token: &str) -> Result<()> {
        let name = target(id);
        let credential = Credential {
            flags: 0,
            kind: GENERIC,
            target_name: name.as_ptr(),
            comment: ptr::null(),
            last_written: [0; 2],
            blob_size: token.len() as u32,
            blob: token.as_ptr(),
            persist: PERSIST_LOCAL_MACHINE,
            attribute_count: 0,
            attributes: ptr::null(),
            target_alias: ptr::null(),
            user_name: ptr::null(),
        };
        ensure!(
            unsafe { CredWriteW(&credential, 0) } != 0,
            "Диспетчер учётных данных Windows: {}",
            std::io::Error::last_os_error()
        );
        Ok(())
    }

    pub fn clear(id: &str) -> Result<()> {
        let name = target(id);
        if unsafe { CredDeleteW(name.as_ptr(), GENERIC, 0) } == 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(NOT_FOUND) {
                return Err(error.into());
            }
        }
        Ok(())
    }
}

pub use os::{clear, load, store};
