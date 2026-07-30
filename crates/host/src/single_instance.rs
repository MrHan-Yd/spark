//! Single-instance guard via named mutex.

use anyhow::{bail, Result};
use spark_core::SINGLE_INSTANCE_MUTEX;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;

pub struct SingleInstance {
    handle: HANDLE,
}

impl SingleInstance {
    /// Returns Ok(guard) if we are the first instance; Err if another host is running.
    pub fn acquire() -> Result<Self> {
        let name: Vec<u16> = SINGLE_INSTANCE_MUTEX
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let handle = unsafe { CreateMutexW(None, true, PCWSTR(name.as_ptr()))? };
        let err = unsafe { GetLastError() };
        if err == ERROR_ALREADY_EXISTS {
            unsafe {
                let _ = CloseHandle(handle);
            }
            bail!("another spark-host instance is already running");
        }
        Ok(Self { handle })
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}
