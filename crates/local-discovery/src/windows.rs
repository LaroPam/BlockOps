use anyhow::{ensure, Result};
use std::{
    fs::OpenOptions,
    io::Read,
    os::windows::{
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawHandle,
    },
    path::Path,
    ptr,
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, LocalFree, HANDLE},
    Security::{
        Authorization::{GetSecurityInfo, SE_FILE_OBJECT},
        EqualSid, GetAce, GetSecurityDescriptorControl, GetTokenInformation, IsValidAcl,
        IsValidSid, TokenUser, ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION,
        OWNER_SECURITY_INFORMATION, SE_DACL_PROTECTED, TOKEN_QUERY, TOKEN_USER,
    },
    Storage::FileSystem::{FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT},
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
struct SecurityDescriptor(*mut core::ffi::c_void);
impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}

pub fn read_private(path: &Path) -> Result<Vec<u8>> {
    // The ACL and contents are checked on the same handle, so replacement of the
    // path after opening cannot change which file supplies the capability.
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
        "Invalid private descriptor file"
    );
    unsafe {
        let mut raw_token = ptr::null_mut();
        ensure!(
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw_token) != 0,
            "Cannot inspect current user"
        );
        let token = Handle(raw_token);
        let mut size = 0;
        GetTokenInformation(token.0, TokenUser, ptr::null_mut(), 0, &mut size);
        ensure!(
            size as usize >= std::mem::size_of::<TOKEN_USER>() && size <= 65536,
            "Cannot inspect current user"
        );
        // usize provides the required pointer alignment for TOKEN_USER.
        let mut user = vec![0usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
        ensure!(
            GetTokenInformation(
                token.0,
                TokenUser,
                user.as_mut_ptr().cast(),
                size,
                &mut size
            ) != 0,
            "Cannot inspect current user"
        );
        let sid = (*(user.as_ptr().cast::<TOKEN_USER>())).User.Sid;
        ensure!(
            !sid.is_null() && IsValidSid(sid) != 0,
            "Cannot inspect current user"
        );
        let mut owner = ptr::null_mut();
        let mut dacl: *mut ACL = ptr::null_mut();
        let mut descriptor = ptr::null_mut();
        let result = GetSecurityInfo(
            file.as_raw_handle() as HANDLE,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            &mut dacl,
            ptr::null_mut(),
            &mut descriptor,
        );
        let descriptor = SecurityDescriptor(descriptor);
        ensure!(
            result == 0 && !descriptor.0.is_null() && !owner.is_null() && !dacl.is_null(),
            "Cannot verify descriptor permissions"
        );
        ensure!(
            IsValidSid(owner) != 0 && EqualSid(owner, sid) != 0 && IsValidAcl(dacl) != 0,
            "Descriptor permissions are not private"
        );
        let mut control = 0;
        let mut revision = 0;
        ensure!(
            GetSecurityDescriptorControl(descriptor.0, &mut control, &mut revision) != 0
                && control & SE_DACL_PROTECTED != 0,
            "Descriptor permissions are not private"
        );
        ensure!(
            (*dacl).AceCount > 0,
            "Descriptor permissions are not private"
        );
        for i in 0..u32::from((*dacl).AceCount) {
            let mut ace = ptr::null_mut();
            ensure!(
                GetAce(dacl, i, &mut ace) != 0 && !ace.is_null(),
                "Descriptor permissions are not private"
            );
            let header = &*ace.cast::<ACE_HEADER>();
            // Accept only plain, non-inherited allow ACEs for this user. Unknown
            // ACE types are rejected rather than interpreted optimistically.
            ensure!(
                header.AceType == 0
                    && header.AceFlags == 0
                    && usize::from(header.AceSize) >= std::mem::size_of::<ACCESS_ALLOWED_ACE>(),
                "Descriptor permissions are not private"
            );
            let allowed = &*ace.cast::<ACCESS_ALLOWED_ACE>();
            let ace_sid = ptr::addr_of!(allowed.SidStart) as *mut core::ffi::c_void;
            ensure!(
                IsValidSid(ace_sid) != 0 && EqualSid(ace_sid, sid) != 0,
                "Descriptor permissions are not private"
            );
        }
    }
    let mut bytes = Vec::new();
    file.take(4097).read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, process::Command};

    fn icacls(path: &Path, args: &[&str]) {
        let status = Command::new("icacls")
            .arg(path)
            .args(args)
            .output()
            .expect("run icacls");
        assert!(
            status.status.success(),
            "icacls failed: {}",
            String::from_utf8_lossy(&status.stderr)
        );
    }

    fn user_sid() -> String {
        let output = Command::new("whoami")
            .args(["/user", "/fo", "csv", "/nh"])
            .output()
            .expect("run whoami");
        assert!(output.status.success());
        let csv = String::from_utf8(output.stdout).unwrap();
        csv.trim()
            .split(',')
            .nth(1)
            .unwrap()
            .trim_matches('"')
            .to_owned()
    }

    #[test]
    fn rejects_broad_and_inherited_permissions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("descriptor.json");
        fs::write(&path, b"synthetic secret").unwrap();
        let sid = user_sid();
        let principal = format!("*{sid}");
        let grant = format!("{principal}:(F)");
        icacls(&path, &["/setowner", &principal]);
        icacls(&path, &["/inheritance:r"]);
        icacls(&path, &["/grant:r", &grant]);
        assert_eq!(read_private(&path).unwrap(), b"synthetic secret");
        icacls(&path, &["/grant", "*S-1-1-0:(R)"]);
        assert!(read_private(&path).is_err());
        icacls(&path, &["/remove:g", "*S-1-1-0"]);
        icacls(&path, &["/inheritance:e"]);
        assert!(read_private(&path).is_err());
    }
}
