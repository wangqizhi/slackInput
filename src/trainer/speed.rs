//! x64 main-module IAT clocks. Remote code stays resident at x1 after detach:
//! reverting to the original clock would make accumulated virtual time jump back.
//! No DLL loading, remote thread, or change to the system clock is required.
use super::*;
use iced_x86::code_asm::*;

pub struct Session {
    process: Process,
    factor: usize,
    pub multiplier: u32,
}
// Access is serialized by AppState's mutex; the handle owns the original process.
unsafe impl Send for Session {}

#[derive(Clone, Copy, Debug)]
enum Clock {
    Counter,
    Tick64,
    Tick32,
}

impl Session {
    pub fn open(target: &crate::trainer::Target) -> Result<Self> {
        if cfg!(not(target_arch = "x86_64")) {
            return Err("游戏加速仅支持 x64 / Speed requires x64".into());
        }
        let process = Process::open(target)?;
        let snapshot = OwnedHandle(
            unsafe {
                CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, target.pid)
            }
            .map_err(error)?,
        );
        let mut module = MODULEENTRY32W {
            dwSize: size_of::<MODULEENTRY32W>() as u32,
            ..Default::default()
        };
        unsafe { Module32FirstW(snapshot.0, &mut module) }.map_err(error)?;
        let base = module.modBaseAddr as usize;
        let size = module.modBaseSize as usize;
        let read = |rva: usize, len: usize| {
            if rva.checked_add(len).is_none_or(|end| end > size) {
                return Err("无效 PE 范围 / Invalid PE range".into());
            }
            process.read(base + rva, len)
        };
        let dos = read(0, 64)?;
        if &dos[..2] != b"MZ" {
            return Err("Invalid DOS header".into());
        }
        let nt = u32::from_le_bytes(dos[60..64].try_into().unwrap()) as usize;
        let header = read(nt, 144)?;
        if &header[..4] != b"PE\0\0"
            || header[4..6] != [0x64, 0x86]
            || header[24..26] != [0x0b, 0x02]
        {
            return Err("游戏加速仅支持原生 x64 游戏 / Native x64 games only".into());
        }
        let imports = read(nt + 144, 8)?;
        let rva = u32::from_le_bytes(imports[..4].try_into().unwrap()) as usize;
        let len = u32::from_le_bytes(imports[4..].try_into().unwrap()) as usize;
        if rva == 0 || !(20..=65536).contains(&len) {
            return Err("未找到支持的计时导入 / No supported clock imports".into());
        }
        let descriptors = read(rva, len)?;
        let mut entries = Vec::new();
        for descriptor in descriptors.chunks_exact(20) {
            if descriptor.iter().all(|v| *v == 0) {
                break;
            }
            let names = u32::from_le_bytes(descriptor[..4].try_into().unwrap()) as usize;
            let slots = u32::from_le_bytes(descriptor[16..20].try_into().unwrap()) as usize;
            if names == 0 || slots == 0 {
                continue;
            }
            for i in 0..8192 {
                let name = u64::from_le_bytes(read(names + i * 8, 8)?.try_into().unwrap());
                if name == 0 {
                    break;
                }
                if name >> 63 != 0 {
                    continue;
                }
                let name = usize::try_from(name).map_err(|e| e.to_string())?;
                let bytes = read(name + 2, 32.min(size.saturating_sub(name + 2)))?;
                let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
                let kind = match &bytes[..end] {
                    b"QueryPerformanceCounter" => Clock::Counter,
                    b"GetTickCount64" => Clock::Tick64,
                    b"GetTickCount" | b"timeGetTime" => Clock::Tick32,
                    _ => continue,
                };
                let slot = base + slots + i * 8;
                if slot % 8 != 0 {
                    return Err("Unaligned import address table".into());
                }
                let original = read(slots + i * 8, 8)?;
                entries.push((slot, original, kind));
            }
        }
        if entries.is_empty() || entries.len() > 32 {
            return Err("该游戏没有支持的计时导入 / No supported clock imports in game".into());
        }
        // A single shared factor makes switching all intercepted clocks consistent.
        let factor = process.allocate_near(base)?;
        process.write(factor, &1u32.to_le_bytes())?;
        let session = Self {
            process,
            factor,
            multiplier: 1,
        };
        for (slot, original, kind) in entries {
            let cave = session.process.allocate_near(base)?;
            let pointer = u64::from_le_bytes(original.as_slice().try_into().unwrap());
            session.process.write(cave + 40, &pointer.to_le_bytes())?;
            let code = assemble(cave, factor, kind).map_err(|e| e.to_string())?;
            session.process.write(cave + 0x1000, &code)?;
            session
                .process
                .protect(cave + 0x1000, 0x1000, PAGE_EXECUTE_READ)?;
            session.process.flush(cave + 0x1000, code.len())?;
            session.process.replace_code(
                slot,
                &original,
                &((cave + 0x1000) as u64).to_le_bytes(),
                &[],
            )?;
            // Once published, never free a cave: another thread may have cached it.
        }
        Ok(session)
    }

    pub fn set(&mut self, multiplier: u32) -> Result<()> {
        if ![1, 2, 4].contains(&multiplier) {
            return Err("Invalid speed".into());
        }
        if !self.process.exited() {
            let result = self.process.paused(&[], || {
                self.process.write(self.factor, &multiplier.to_le_bytes())
            });
            // Even a failed resume can follow a successful factor write.
            if let Ok(bytes) = self.process.read(self.factor, 4) {
                let actual = u32::from_le_bytes(bytes.try_into().unwrap());
                if [1, 2, 4].contains(&actual) {
                    self.multiplier = actual;
                }
            }
            result?;
        }
        self.multiplier = multiplier;
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.set(1);
    }
}

fn assemble(
    data: usize,
    factor: usize,
    kind: Clock,
) -> std::result::Result<Vec<u8>, iced_x86::IcedError> {
    let mut a = CodeAssembler::new(64)?;
    let mut acquire = a.create_label();
    let mut first = a.create_label();
    let mut output = a.create_label();
    let mut release = a.create_label();
    a.push(rbx)?;
    a.push(r12)?;
    a.sub(rsp, 40)?; // 32-byte shadow space + QPC result; keep stack aligned.
    a.mov(rbx, rcx)?;
    a.mov(r12, data as u64)?;
    a.set_label(&mut acquire)?;
    a.mov(eax, 1)?;
    a.xchg(dword_ptr(r12), eax)?;
    a.test(eax, eax)?;
    a.je(first)?;
    a.pause()?;
    a.jmp(acquire)?;
    a.set_label(&mut first)?;
    if matches!(kind, Clock::Counter) {
        a.lea(rcx, ptr(rsp + 32))?;
    }
    a.call(qword_ptr(r12 + 40))?;
    if matches!(kind, Clock::Counter) {
        a.test(eax, eax)?;
        a.je(release)?; // Preserve a failed QPC result without touching output.
        a.mov(rax, qword_ptr(rsp + 32))?;
    } else if matches!(kind, Clock::Tick32) {
        a.mov(eax, eax)?;
    }
    a.mov(rdx, rax)?;
    let mut initialized = a.create_label();
    a.cmp(dword_ptr(r12 + 16), 0)?;
    a.jne(initialized)?;
    a.mov(dword_ptr(r12 + 16), 1)?;
    a.mov(qword_ptr(r12 + 32), rax)?;
    a.jmp(output)?;
    a.set_label(&mut initialized)?;
    a.sub(rax, qword_ptr(r12 + 24))?;
    if matches!(kind, Clock::Tick32) {
        a.mov(eax, eax)?;
    } // DWORD wraparound.
    a.mov(r11, factor as u64)?;
    a.mov(r11d, dword_ptr(r11))?;
    a.imul_2(rax, r11)?;
    a.add(rax, qword_ptr(r12 + 32))?;
    a.mov(qword_ptr(r12 + 32), rax)?;
    a.set_label(&mut output)?;
    a.mov(qword_ptr(r12 + 24), rdx)?;
    if matches!(kind, Clock::Counter) {
        a.mov(qword_ptr(rbx), rax)?;
        a.mov(eax, 1)?;
    }
    a.set_label(&mut release)?;
    a.mov(dword_ptr(r12), 0)?;
    a.add(rsp, 40)?;
    a.pop(r12)?;
    a.pop(rbx)?;
    a.ret()?;
    a.assemble((data + 0x1000) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static RAW: AtomicU64 = AtomicU64::new(0);
    unsafe extern "system" fn tick() -> u64 {
        RAW.load(Ordering::SeqCst)
    }
    unsafe extern "system" fn counter(out: *mut u64) -> i32 {
        unsafe {
            *out = RAW.load(Ordering::SeqCst);
        }
        1
    }

    // Execute the generated instructions on isolated local memory, not a game.
    // One test owns RAW so cargo's parallel test runner cannot race fixtures.
    #[test]
    fn clock_code_scales_restores_and_handles_dword_wrap() {
        for kind in [Clock::Counter, Clock::Tick64, Clock::Tick32] {
            let memory =
                unsafe { VirtualAlloc(None, 0x2000, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE) };
            assert!(!memory.is_null());
            struct Allocation(*mut std::ffi::c_void);
            impl Drop for Allocation {
                fn drop(&mut self) {
                    let _ = unsafe { VirtualFree(self.0, 0, MEM_RELEASE) };
                }
            }
            let _allocation = Allocation(memory);
            let base = memory as usize;
            let factor = AtomicU64::new(1);
            let original = if matches!(kind, Clock::Counter) {
                counter as *const () as u64
            } else {
                tick as *const () as u64
            };
            unsafe {
                ((base + 40) as *mut u64).write(original);
            }
            let code = assemble(base, factor.as_ptr() as usize, kind).unwrap();
            unsafe {
                std::ptr::copy_nonoverlapping(
                    code.as_ptr(),
                    (base + 0x1000) as *mut u8,
                    code.len(),
                );
            }
            let mut old = PAGE_PROTECTION_FLAGS::default();
            unsafe {
                VirtualProtect((base + 0x1000) as _, 0x1000, PAGE_EXECUTE_READ, &mut old).unwrap();
                FlushInstructionCache(GetCurrentProcess(), Some((base + 0x1000) as _), code.len())
                    .unwrap();
            }
            let invoke = || unsafe {
                if matches!(kind, Clock::Counter) {
                    let function: unsafe extern "system" fn(*mut u64) -> i32 =
                        std::mem::transmute(base + 0x1000);
                    let mut value = 0;
                    assert_eq!(function(&mut value), 1);
                    value
                } else if matches!(kind, Clock::Tick32) {
                    let function: unsafe extern "system" fn() -> u32 =
                        std::mem::transmute(base + 0x1000);
                    function() as u64
                } else {
                    let function: unsafe extern "system" fn() -> u64 =
                        std::mem::transmute(base + 0x1000);
                    function()
                }
            };
            RAW.store(100, Ordering::SeqCst);
            assert_eq!(invoke(), 100);
            factor.store(2, Ordering::SeqCst);
            RAW.store(110, Ordering::SeqCst);
            assert_eq!(invoke(), 120);
            factor.store(4, Ordering::SeqCst);
            RAW.store(120, Ordering::SeqCst);
            assert_eq!(invoke(), 160);
            factor.store(1, Ordering::SeqCst);
            assert_eq!(invoke(), 160); // Returning to x1 does not jump backwards.
            RAW.store(130, Ordering::SeqCst);
            assert_eq!(invoke(), 170);
            if matches!(kind, Clock::Tick32) {
                unsafe {
                    ((base + 24) as *mut u64).write(u32::MAX as u64 - 4);
                    ((base + 32) as *mut u64).write(u32::MAX as u64 - 4);
                }
                factor.store(2, Ordering::SeqCst);
                RAW.store(5, Ordering::SeqCst);
                assert_eq!(invoke(), 15);
            }
        }
    }
}
