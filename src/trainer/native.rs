//! Small, explicit native adapter; never executes imported assembler/Lua/DLLs.
//! Recipes correspond to the fingerprinted sample documented in docs/trainer-development.md.
use super::{Target, hooks::{self, Group}, profile::{self, GAME_PROCESS}};
use std::{
    collections::BTreeMap,
    mem::size_of,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use windows::Win32::{
    Foundation::{CloseHandle, ERROR_NO_MORE_FILES, FILETIME, HANDLE, NTSTATUS, WAIT_OBJECT_0},
    System::{
        Diagnostics::{
            Debug::{
                CONTEXT, CONTEXT_CONTROL_AMD64, FlushInstructionCache, GetThreadContext,
                ReadProcessMemory, WriteProcessMemory,
            },
            ToolHelp::*,
        },
        Memory::*,
        Threading::*,
    },
};

type Result<T> = std::result::Result<T, String>;
fn error(e: windows::core::Error) -> String {
    e.to_string()
}
#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtSuspendProcess(handle: HANDLE) -> NTSTATUS;
    fn NtResumeProcess(handle: HANDLE) -> NTSTATUS;
}

struct OwnedHandle(HANDLE);
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

pub fn creation_time(handle: HANDLE) -> Result<u64> {
    let (mut created, mut exited, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) }
        .map_err(error)?;
    Ok((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
}

struct Process {
    handle: OwnedHandle,
    pid: u32,
}
impl Process {
    fn open(target: &Target) -> Result<Self> {
        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_INFORMATION
                    | PROCESS_VM_READ
                    | PROCESS_VM_WRITE
                    | PROCESS_VM_OPERATION
                    | PROCESS_SUSPEND_RESUME
                    | PROCESS_SYNCHRONIZE,
                false,
                target.pid,
            )
        }
        .map_err(error)?;
        let result = Self {
            handle: OwnedHandle(handle),
            pid: target.pid,
        };
        if creation_time(handle)? != target.created || result.exited() {
            return Err("进程已退出或身份改变".into());
        }
        Ok(result)
    }
    fn exited(&self) -> bool {
        unsafe { WaitForSingleObject(self.handle.0, 0) == WAIT_OBJECT_0 }
    }
    fn read(&self, address: usize, length: usize) -> Result<Vec<u8>> {
        if length > 256 * 1024 * 1024 {
            return Err("读取范围过大".into());
        }
        let mut bytes = vec![0; length];
        let mut count = 0;
        unsafe {
            ReadProcessMemory(
                self.handle.0,
                address as _,
                bytes.as_mut_ptr().cast(),
                length,
                Some(&mut count),
            )
        }
        .map_err(error)?;
        if count != length {
            return Err("读取不完整".into());
        }
        Ok(bytes)
    }
    fn write(&self, address: usize, bytes: &[u8]) -> Result<()> {
        let mut count = 0;
        unsafe {
            WriteProcessMemory(
                self.handle.0,
                address as _,
                bytes.as_ptr().cast(),
                bytes.len(),
                Some(&mut count),
            )
        }
        .map_err(error)?;
        if count != bytes.len() || self.read(address, bytes.len())? != bytes {
            return Err("写入校验失败".into());
        }
        Ok(())
    }
    fn protect(
        &self,
        address: usize,
        length: usize,
        flags: PAGE_PROTECTION_FLAGS,
    ) -> Result<PAGE_PROTECTION_FLAGS> {
        let mut old = PAGE_PROTECTION_FLAGS::default();
        unsafe { VirtualProtectEx(self.handle.0, address as _, length, flags, &mut old) }
            .map_err(error)?;
        Ok(old)
    }
    fn flush(&self, address: usize, length: usize) -> Result<()> {
        unsafe { FlushInstructionCache(self.handle.0, Some(address as _), length) }.map_err(error)
    }
    fn paused<T>(
        &self,
        ranges: &[(usize, usize)],
        action: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        #[cfg(test)]
        if self.pid == std::process::id() {
            return action();
        }
        unsafe { NtSuspendProcess(self.handle.0).ok() }.map_err(error)?;
        struct Resume(HANDLE, bool);
        impl Drop for Resume {
            fn drop(&mut self) {
                if self.1 {
                    let _ = unsafe { NtResumeProcess(self.0) };
                }
            }
        }
        let mut guard = Resume(self.handle.0, true);
        let result = self.check_threads(ranges).and_then(|_| action());
        unsafe { NtResumeProcess(self.handle.0).ok() }
            .map_err(|e| format!("恢复临时暂停失败：{e}"))?;
        guard.1 = false;
        result
    }
    fn check_threads(&self, ranges: &[(usize, usize)]) -> Result<()> {
        let snapshot =
            OwnedHandle(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }.map_err(error)?);
        let mut entry = THREADENTRY32 {
            dwSize: size_of::<THREADENTRY32>() as u32,
            ..Default::default()
        };
        unsafe { Thread32First(snapshot.0, &mut entry) }.map_err(error)?;
        loop {
            if entry.th32OwnerProcessID == self.pid {
                let thread = OwnedHandle(
                    unsafe {
                        OpenThread(
                            THREAD_GET_CONTEXT | THREAD_QUERY_INFORMATION,
                            false,
                            entry.th32ThreadID,
                        )
                    }
                    .map_err(error)?,
                );
                // Windows requires 16-byte alignment; CONTEXT's ABI provides it.
                let mut context = CONTEXT {
                    ContextFlags: CONTEXT_CONTROL_AMD64,
                    ..Default::default()
                };
                unsafe { GetThreadContext(thread.0, &mut context) }.map_err(error)?;
                if ranges
                    .iter()
                    .any(|&(start, len)| (start..start + len).contains(&(context.Rip as usize)))
                {
                    return Err("游戏线程正在执行该修改区域，请稍后重试".into());
                }
            }
            if let Err(e) = unsafe { Thread32Next(snapshot.0, &mut entry) } {
                if e.code() != ERROR_NO_MORE_FILES.to_hresult() {
                    return Err(error(e));
                }
                break;
            }
        }
        Ok(())
    }
    fn replace_code(
        &self,
        address: usize,
        from: &[u8],
        to: &[u8],
        extra: &[(usize, usize)],
    ) -> Result<()> {
        if from.len() != to.len() {
            return Err("补丁长度不匹配".into());
        }
        if (address & 0xfff) + to.len() > 0x1000 {
            return Err("补丁跨越内存页边界，已停止修改".into());
        }
        let mut ranges = vec![(address, from.len())];
        ranges.extend_from_slice(extra);
        self.paused(&ranges, || {
            let actual = self.read(address, from.len())?;
            if actual != from {
                return Err("目标代码与预期不同，可能版本不兼容或存在其他修改器".into());
            }
            let old = self.protect(address, to.len(), PAGE_EXECUTE_READWRITE)?;
            let operation = self
                .write(address, to)
                .and_then(|_| self.flush(address, to.len()));
            if operation.is_err() {
                let rollback = self
                    .write(address, from)
                    .and_then(|_| self.flush(address, from.len()));
                if let Err(e) = rollback {
                    let _ = self.protect(address, to.len(), old);
                    return Err(format!("写入及回滚失败，需退出游戏：{e}"));
                }
            }
            let protection = self.protect(address, to.len(), old);
            if let Err(e) = protection {
                // Don't claim disabled when changed bytes could remain installed.
                let _ = self.write(address, from);
                let _ = self.flush(address, from.len());
                let _ = self.protect(address, from.len(), old);
                return Err(format!("恢复内存保护失败：{e}"));
            }
            operation
        })
    }
    fn allocate_near(&self, address: usize) -> Result<usize> {
        // Windows allocation granularity is 64 KiB. Stay well within signed rel32 range.
        let aligned = address & !0xffff;
        for distance in (0x10000..0x70000000usize).step_by(0x10000) {
            for candidate in [aligned.checked_add(distance), aligned.checked_sub(distance)]
                .into_iter()
                .flatten()
            {
                if candidate < 0x10000 {
                    continue;
                }
                let ptr = unsafe {
                    VirtualAllocEx(
                        self.handle.0,
                        Some(candidate as _),
                        0x2000,
                        MEM_RESERVE | MEM_COMMIT,
                        PAGE_READWRITE,
                    )
                };
                if !ptr.is_null() {
                    return Ok(ptr as usize);
                }
            }
        }
        Err("无法在目标代码附近分配内存".into())
    }
}

#[derive(Clone)]
struct Pattern(Vec<Option<u8>>);
impl Pattern {
    fn parse(text: &str) -> Result<Self> {
        let bytes: Result<Vec<_>> = text
            .split_whitespace()
            .map(|s| {
                if s == "*" || s == "??" {
                    Ok(None)
                } else {
                    u8::from_str_radix(s, 16)
                        .map(Some)
                        .map_err(|_| "无效扫描特征".into())
                }
            })
            .collect();
        let bytes = bytes?;
        if bytes.len() < 4 || bytes.len() > 256 || bytes.iter().all(Option::is_none) {
            return Err("扫描特征过短或无效".into());
        }
        Ok(Self(bytes))
    }
    fn matches(&self, data: &[u8]) -> Vec<usize> {
        let anchor = self.0.iter().position(Option::is_some).unwrap();
        data.windows(self.0.len())
            .enumerate()
            .filter_map(|(i, w)| {
                (Some(w[anchor]) == self.0[anchor]
                    && self.0.iter().zip(w).all(|(a, b)| a.is_none_or(|a| a == *b)))
                .then_some(i)
            })
            .collect()
    }
}

struct Recipe {
    pattern: &'static str,
    offset: usize,
    original: &'static [u8],
    replacement: &'static [u8],
}
fn recipes(id: &str) -> Vec<Recipe> {
    match id {
        "stealth_mode" => vec![Recipe {
            pattern: "48 8B * * * 00 00 * 85 * 75 07 32 C0 E9 * * 00 00 0F 57",
            offset: 10,
            original: &[0x75, 0x07, 0x32, 0xc0],
            replacement: &[0xb0, 0x01, 0x66, 0x90],
        }],
        "scan_rate_wont_decrease" => [
            "66 44 89 66 02 E8 * * * * * 8D",
            "66 44 89 66 02 * 8D * * * E8",
        ]
        .into_iter()
        .map(|pattern| Recipe {
            pattern,
            offset: 0,
            original: &[0x66, 0x44, 0x89, 0x66, 0x02],
            replacement: &[0x0f, 0x1f, 0x44, 0x00, 0x00],
        })
        .collect(),
        "battle_items_wont_decrease" => vec![Recipe {
            pattern: "E8 * * * * 8B * * 85 * 0F 84 * * 00 00 80 * * * 00 00 00 0F 85 * * 00 00",
            offset: 10,
            original: &[0x0f, 0x84],
            replacement: &[0x90, 0xe9],
        }],
        _ => vec![],
    }
}

struct Patch {
    address: usize,
    original: Vec<u8>,
    replacement: Vec<u8>,
}
struct MoneyHook {
    patch: Patch,
    cave: usize,
    installed: bool,
}
struct GroupHook {
    patch: Patch,
    cave: usize,
    installed: bool,
}

pub struct Session {
    process: Process,
    sections: Vec<(usize, usize)>,
    patches: BTreeMap<String, Patch>,
    money: Option<MoneyHook>,
    hooks: BTreeMap<Group, GroupHook>,
    pub active: BTreeMap<String, Option<f64>>,
    pub applied: BTreeMap<String, (f64, u32)>,
    failure_requires_cleanup: bool,
    cancel: Arc<AtomicU64>,
    epoch: u64,
}

impl Session {
    pub fn open(target: &Target, cancel: Arc<AtomicU64>, epoch: u64) -> Result<Self> {
        if !target.name.eq_ignore_ascii_case(GAME_PROCESS) {
            return Err("当前进程没有执行适配".into());
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
        let name = String::from_utf16_lossy(
            &module.szModule[..module
                .szModule
                .iter()
                .position(|c| *c == 0)
                .unwrap_or(module.szModule.len())],
        );
        if !name.eq_ignore_ascii_case(GAME_PROCESS) {
            return Err("主模块与绑定的游戏不一致".into());
        }
        let base = module.modBaseAddr as usize;
        let image_size = module.modBaseSize as usize;
        let dos = process.read(base, 64)?;
        if &dos[..2] != b"MZ" {
            return Err("无效游戏模块".into());
        }
        let pe_offset = u32::from_le_bytes(dos[60..64].try_into().unwrap()) as usize;
        if pe_offset > 1024 * 1024 || pe_offset + 264 > image_size {
            return Err("无效 PE 头".into());
        }
        let pe = process.read(base + pe_offset, 264)?;
        if &pe[..4] != b"PE\0\0" || u16::from_le_bytes(pe[4..6].try_into().unwrap()) != 0x8664 {
            return Err("仅支持此游戏的 x64 版本".into());
        }
        let count = u16::from_le_bytes(pe[6..8].try_into().unwrap()) as usize;
        let optional = u16::from_le_bytes(pe[20..22].try_into().unwrap()) as usize;
        if count == 0
            || count > 96
            || optional > 4096
            || pe_offset + 24 + optional + count * 40 > image_size
        {
            return Err("无效节区表".into());
        }
        let table = process.read(base + pe_offset + 24 + optional, count * 40)?;
        let mut sections = vec![];
        for s in table.chunks_exact(40) {
            let length = u32::from_le_bytes(s[8..12].try_into().unwrap()) as usize;
            let rva = u32::from_le_bytes(s[12..16].try_into().unwrap()) as usize;
            let flags = u32::from_le_bytes(s[36..40].try_into().unwrap());
            if flags & 0x20000000 != 0 && length > 0 {
                if length > 256 * 1024 * 1024
                    || rva.checked_add(length).is_none_or(|end| end > image_size)
                {
                    return Err("游戏代码节范围无效".into());
                }
                sections.push((base + rva, length));
            }
        }
        if sections.is_empty() {
            return Err("未找到游戏代码节".into());
        }
        Ok(Self {
            process,
            sections,
            patches: BTreeMap::new(),
            money: None,
            hooks: BTreeMap::new(),
            active: BTreeMap::new(),
            applied: BTreeMap::new(),
            failure_requires_cleanup: false,
            cancel,
            epoch,
        })
    }
    pub fn exited(&self) -> bool {
        self.process.exited()
    }
    fn check_cancel(&self) -> Result<()> {
        if self.cancel.load(Ordering::Acquire) != self.epoch || self.exited() {
            Err("操作取消：绑定已变化或游戏已退出".into())
        } else {
            Ok(())
        }
    }
    fn scan(&self, pattern: &Pattern) -> Result<Vec<usize>> {
        let mut found = vec![];
        for &(base, length) in &self.sections {
            let mut done = 0;
            while done < length {
                self.check_cancel()?;
                let size = (length - done).min(1024 * 1024);
                let bytes = self.process.read(base + done, size)?;
                for offset in pattern.matches(&bytes) {
                    found.push(base + done + offset);
                    if found.len() > 1 {
                        return Err("扫描特征存在多个匹配，已停止修改".into());
                    }
                }
                if done + size == length {
                    break;
                }
                done += size - (pattern.0.len() - 1);
            }
        }
        Ok(found)
    }
    fn locate(&self, variants: &[Recipe]) -> Result<(usize, usize)> {
        let mut found = vec![];
        for (index, recipe) in variants.iter().enumerate() {
            for address in self.scan(&Pattern::parse(recipe.pattern)?)? {
                found.push((address + recipe.offset, index));
            }
        }
        found.sort_by_key(|v| v.0);
        found.dedup_by_key(|v| v.0);
        match found.as_slice() {
            [value] => Ok(*value),
            [] => Err("未找到适配特征；此游戏版本暂不支持该功能".into()),
            _ => Err("多个版本特征同时匹配，已停止修改".into()),
        }
    }
    pub fn needs_cleanup(&self) -> bool { self.failure_requires_cleanup }
    pub fn set(&mut self, id: &str, value: Option<f64>, enabled: bool) -> Result<()> {
        if !enabled {
            return self.disable(id);
        }
        self.check_cancel()?;
        let catalog = profile::builtin();
        let feature = catalog.features.iter().find(|f| f.id == id).ok_or("未知修改项")?;
        feature.parse_value(&value.map(|v| v.to_string()).unwrap_or_default())?;
        if matches!(id, "money" | "agent_points") {
            let value = value
                .filter(|v| *v > 0.0)
                .ok_or("请输入 1–2147483647 的整数")?;
            return self.set_money(id, value as i32);
        }
        if let Some(group) = hooks::group(id) {
            if group == Group::Stats { return self.apply_stat(id, value.ok_or("缺少属性数值")? as i32); }
            return self.set_group(group, id, value);
        }
        if self.active.contains_key(id) {
            return Ok(());
        }
        let variants = recipes(id);
        if variants.is_empty() {
            return Err("该功能执行适配尚未实现".into());
        }
        let (address, index) = self.locate(&variants)?;
        let recipe = &variants[index];
        self.check_cancel()?;
        let patch = Patch {
            address,
            original: recipe.original.to_vec(),
            replacement: recipe.replacement.to_vec(),
        };
        self.patches.insert(id.to_string(), patch);
        let patch = &self.patches[id];
        self.failure_requires_cleanup = true;
        self.process
            .replace_code(address, &patch.original, &patch.replacement, &[])?;
        self.active.insert(id.to_string(), None);
        self.failure_requires_cleanup = false;
        Ok(())
    }
    fn set_money(&mut self, id: &str, value: i32) -> Result<()> {
        if self.money.is_none() {
            let variants = [
                Recipe {
                    pattern: "F3 0F 5A * F2 0F 58 4B 50 F2 0F 11",
                    offset: 4,
                    original: &[],
                    replacement: &[],
                },
                Recipe {
                    pattern: "F2 0F 58 4B 50 66 0F 2F * * * * * F2 0F 11",
                    offset: 0,
                    original: &[],
                    replacement: &[],
                },
            ];
            let (address, _) = self.locate(&variants)?;
            self.check_cancel()?;
            let cave = self.process.allocate_near(address)?;
            let code = money_code(cave, address + 5)?;
            let initialize = self
                .process
                .write(cave, &code)
                .and_then(|_| self.process.protect(cave, 0x1000, PAGE_EXECUTE_READ))
                .and_then(|_| self.process.flush(cave, code.len()));
            if let Err(e) = initialize {
                let _ = unsafe { VirtualFreeEx(self.process.handle.0, cave as _, 0, MEM_RELEASE) };
                return Err(e);
            }
            self.money = Some(MoneyHook {
                patch: Patch {
                    address,
                    original: vec![0xf2, 0x0f, 0x58, 0x4b, 0x50],
                    replacement: relative_jump(address, cave)?,
                },
                cave,
                installed: false,
            });
        }
        self.check_cancel()?;
        let hook = self.money.as_ref().unwrap();
        self.failure_requires_cleanup = true;
        if !hook.installed {
            if group == Group::Stats {
                self.process.write(hook.cave + 0x1000, &[0; 12])?;
            }
            self.money.as_mut().unwrap().installed = true;
            let hook = self.money.as_ref().unwrap();
            self.process.replace_code(
                hook.patch.address,
                &hook.patch.original,
                &hook.patch.replacement,
                &[(hook.cave, 0x1000)],
            )?;
        }
        let hook = self.money.as_ref().unwrap();
        if self.process.read(hook.patch.address, hook.patch.replacement.len())? != hook.patch.replacement {
            return Err("金钱/点数代码已被其他程序改变，请先停用全部".into());
        }
        let offset = if id == "money" { 0x1000 } else { 0x1004 };
        self.process
            .write(hook.cave + offset, &value.to_le_bytes())?;
        self.active.insert(id.into(), Some(f64::from(value)));
        self.failure_requires_cleanup = false;
        Ok(())
    }
    fn ensure_group(&mut self, group: Group) -> Result<()> {
        if !self.hooks.contains_key(&group) {
            let variants = group.variants();
            let recipes: Vec<_> = variants.iter().map(|v| Recipe { pattern: v.pattern, offset: v.offset, original: &[], replacement: &[] }).collect();
            let (address, index) = self.locate(&recipes)?;
            let expected = Pattern::parse(variants[index].original)?;
            let original = self.process.read(address, expected.0.len())?;
            if expected.matches(&original) != [0] { return Err("目标原指令不匹配".into()); }
            hooks::validate_original(group, &original)?;
            if let Some(pattern) = group.layout_pattern() {
                if self.scan(&Pattern::parse(pattern)?)?.len() != 1 { return Err("游戏字段布局校验失败，此版本暂不支持".into()); }
            }
            self.check_cancel()?;
            let cave = self.process.allocate_near(address)?;
            let initialize = (|| {
                let code = hooks::build(group, &original, cave, address + original.len())?;
                if code.len() > 0x1000 { return Err("适配代码超出分配范围".into()); }
                self.process.write(cave, &code)?;
                if group == Group::Bond { self.process.write(cave + 0x1104, &9999999f32.to_le_bytes())?; }
                self.process.protect(cave, 0x1000, PAGE_EXECUTE_READ)?;
                self.process.flush(cave, code.len())?;
                let mut replacement = relative_jump(address, cave)?;
                replacement.resize(original.len(), 0x90);
                Ok(replacement)
            })();
            let replacement = match initialize {
                Ok(v) => v,
                Err(e) => { let _ = unsafe { VirtualFreeEx(self.process.handle.0, cave as _, 0, MEM_RELEASE) }; return Err(e); }
            };
            self.hooks.insert(group, GroupHook { patch: Patch { address, original, replacement }, cave, installed: false });
        }
        self.check_cancel()?;
        let hook = &self.hooks[&group];
        if !hook.installed {
            self.failure_requires_cleanup = true;
            self.hooks.get_mut(&group).unwrap().installed = true;
            let hook = &self.hooks[&group];
            self.process.replace_code(hook.patch.address, &hook.patch.original, &hook.patch.replacement, &[(hook.cave, 0x1000)])?;
            self.failure_requires_cleanup = false;
        } else if self.process.read(hook.patch.address, hook.patch.replacement.len())? != hook.patch.replacement {
            self.failure_requires_cleanup = true;
            return Err("修改代码已被其他程序改变，请先停用全部".into());
        }
        Ok(())
    }
    fn set_group(&mut self, group: Group, id: &str, value: Option<f64>) -> Result<()> {
        self.ensure_group(group)?;
        self.check_cancel()?;
        let index = group.parameters().iter().position(|s| *s == id).ok_or("未知脚本参数")?;
        let bytes = if id.ends_with("_f") {
            (value.ok_or("缺少倍率")? as f32).to_le_bytes()
        } else { (value.unwrap_or(1.0) as i32).to_le_bytes() };
        let address = self.hooks[&group].cave + 0x1000 + index * 4;
        self.failure_requires_cleanup = true;
        self.process.paused(&[], || self.process.write(address, &bytes))?;
        self.active.insert(id.into(), value);
        self.failure_requires_cleanup = false;
        Ok(())
    }
    fn apply_stat(&mut self, id: &str, value: i32) -> Result<()> {
        let writes = hooks::stat_writes(id, value)?;
        self.ensure_group(Group::Stats)?;
        self.check_cancel()?;
        let cave = self.hooks[&Group::Stats].cave;
        // The capture hook starts empty. Never queue a write for whichever creature
        // might happen to be viewed later; the user must explicitly apply again.
        if self.process.read(cave + 0x1000, 8)? == [0;8] {
            return Err("已准备属性读取。请在游戏中打开或重新打开数码宝贝属性界面，再点击“应用”".into());
        }
        let mut write_started = false;
        let result = self.process.paused(&[(cave, 0x1000)], || {
            let capture = self.process.read(cave + 0x1000, 12)?;
            let pointer = u64::from_le_bytes(capture[..8].try_into().unwrap()) as usize;
            let captured_id = u32::from_le_bytes(capture[8..12].try_into().unwrap());
            if pointer < 0x10000 || pointer > 0x00007fff_ffffe000 || captured_id == 0 {
                return Err("尚未读取到有效数码宝贝，请重新打开属性界面".into());
            }
            if self.process.read(pointer + 4, 4)? != captured_id.to_le_bytes() {
                return Err("数码宝贝对象已变化，请重新打开属性界面".into());
            }
            let originals: Result<Vec<_>> = writes.iter().map(|(offset,_)| self.process.read(pointer + offset, 4)).collect();
            let originals = originals?;
            write_started = true;
            for (index, (offset, bytes)) in writes.iter().enumerate() {
                if let Err(e) = self.process.write(pointer + offset, bytes) {
                    let mut rollback_errors = vec![];
                    for j in 0..=index {
                        if let Err(e) = self.process.write(pointer + writes[j].0, &originals[j]) { rollback_errors.push(e); }
                    }
                    return Err(format!("属性写入失败：{e}{}", if rollback_errors.is_empty() { String::new() } else { format!("；回滚失败：{}", rollback_errors.join("；")) }));
                }
            }
            Ok(captured_id)
        });
        match result {
            Ok(captured_id) => { self.applied.insert(id.into(), (f64::from(value), captured_id)); Ok(()) }
            Err(e) => { self.failure_requires_cleanup = write_started; Err(e) }
        }
    }
    fn restore_group(&mut self, group: Group) -> Result<()> {
        if let Some(hook) = self.hooks.get(&group) {
            if hook.installed {
                self.failure_requires_cleanup = true;
                if self.process.read(hook.patch.address, hook.patch.original.len())? != hook.patch.original {
                    self.process.replace_code(hook.patch.address, &hook.patch.replacement, &hook.patch.original, &[(hook.cave, 0x1000)])?;
                }
                self.hooks.get_mut(&group).unwrap().installed = false;
            }
        }
        Ok(())
    }
    fn disable(&mut self, id: &str) -> Result<()> {
        if self.exited() {
            self.active.clear();
            self.patches.clear();
            return Ok(());
        }
        if let Some(group) = hooks::group(id) {
            if let Some(hook) = self.hooks.get(&group) {
                self.failure_requires_cleanup = true;
                if let Some(index) = group.parameters().iter().position(|s| *s == id) {
                    self.process.paused(&[], || self.process.write(hook.cave + 0x1000 + index * 4, &0u32.to_le_bytes()))?;
                }
                let shared = group.parameters().iter().any(|other| *other != id && self.active.contains_key(*other));
                if !shared { self.restore_group(group)?; }
            }
        } else if matches!(id, "money" | "agent_points") {
            if let Some(hook) = &self.money {
                self.process.write(
                    hook.cave + if id == "money" { 0x1000 } else { 0x1004 },
                    &0i32.to_le_bytes(),
                )?;
                let other = if id == "money" {
                    "agent_points"
                } else {
                    "money"
                };
                if hook.installed && !self.active.contains_key(other) {
                    if self
                        .process
                        .read(hook.patch.address, hook.patch.original.len())?
                        != hook.patch.original
                    {
                        self.process.replace_code(
                            hook.patch.address,
                            &hook.patch.replacement,
                            &hook.patch.original,
                            &[(hook.cave, 0x1000)],
                        )?;
                    }
                    self.money.as_mut().unwrap().installed = false;
                }
            }
        } else if let Some(patch) = self.patches.get(id) {
            if self.process.read(patch.address, patch.original.len())? != patch.original {
                self.process.replace_code(
                    patch.address,
                    &patch.replacement,
                    &patch.original,
                    &[],
                )?;
            }
            self.patches.remove(id);
        }
        self.active.remove(id);
        self.failure_requires_cleanup = false;
        Ok(())
    }
    pub fn cleanup(&mut self) -> Result<()> {
        if self.exited() {
            self.active.clear();
            self.patches.clear();
            self.applied.clear();
            self.failure_requires_cleanup = false;
            return Ok(());
        }
        let mut errors = vec![];
        let mut ids: Vec<_> = self
            .active
            .keys()
            .chain(self.patches.keys())
            .cloned()
            .collect();
        ids.sort();
        ids.dedup();
        for id in ids {
            if let Err(e) = self.disable(&id) {
                errors.push(format!("{id}: {e}"));
            }
        }
        // A parameter write can fail after hook installation, before an active flag exists.
        if self.money.as_ref().is_some_and(|h| h.installed)
            && !self.active.contains_key("money")
            && !self.active.contains_key("agent_points")
        {
            if let Err(e) = self.disable("money") {
                errors.push(e);
            }
        }
        // Includes a stats capture hook with no enabled toggle, and any journaled
        // installation that failed before publishing an active UI state.
        for group in self.hooks.keys().copied().collect::<Vec<_>>() {
            match self.restore_group(group) {
                Ok(()) => { for id in group.parameters() { self.active.remove(*id); } }
                Err(e) => errors.push(e),
            }
        }
        if errors.is_empty() {
            self.failure_requires_cleanup = false;
            Ok(())
        } else {
            self.failure_requires_cleanup = true;
            Err(errors.join("\n"))
        }
    }
}

#[cfg(test)]
#[path = "native_tests.rs"]
mod recipe_tests;

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.cleanup();
        // Do not release a cave while a thread could have a return address into it.
        // At most 8 KiB per hook group/session; reclaimed when the game exits.
    }
}

fn relative_jump(from: usize, to: usize) -> Result<Vec<u8>> {
    let displacement = i32::try_from(to as i128 - from as i128 - 5).map_err(|_| "跳转超出范围")?;
    let mut bytes = vec![0xe9];
    bytes.extend_from_slice(&displacement.to_le_bytes());
    Ok(bytes)
}

fn money_code(base: usize, return_address: usize) -> Result<Vec<u8>> {
    // Preserve flags and rdx, update the two requested fields, replay original addsd.
    // Parameters occupy a separate RW page; the instruction page is RX.
    let mut code = vec![0x9c, 0x52]; // pushfq; push rdx
    for (parameter, field) in [(base + 0x1000, 0x58), (base + 0x1004, 0x5c)] {
        code.extend_from_slice(&[0x8b, 0x15]); // mov edx, [rip+disp32]
        let displacement = i32::try_from(parameter as i128 - (base + code.len() + 4) as i128)
            .map_err(|_| "参数超出范围")?;
        code.extend_from_slice(&displacement.to_le_bytes());
        code.extend_from_slice(&[0x85, 0xd2, 0x7e, 0x03, 0x89, 0x53, field]); // test; jle; mov [rbx+field],edx
    }
    code.extend_from_slice(&[0x5a, 0x9d, 0xf2, 0x0f, 0x58, 0x4b, 0x50]);
    code.extend(relative_jump(base + code.len(), return_address)?);
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn patterns_reject_ambiguity_and_jumps_check_range() {
        let p = Pattern::parse("01 02 * 04").unwrap();
        assert_eq!(p.matches(&[1, 2, 3, 4, 1, 2, 5, 4]), vec![0, 4]);
        assert!(Pattern::parse("* * * *").is_err());
        assert!(Pattern::parse("s1.2 00 00 01").is_err());
        assert!(relative_jump(0x1000, 0x90000000).is_err());
        assert_eq!(
            relative_jump(0x1000, 0x1100).unwrap(),
            vec![0xe9, 0xfb, 0, 0, 0]
        );
    }

    #[test]
    fn all_patch_recipes_scan_enable_restore_and_reject_multiple_matches() {
        for id in [
            "stealth_mode",
            "scan_rate_wont_decrease",
            "battle_items_wont_decrease",
        ] {
            for recipe in recipes(id) {
                let handle =
                    unsafe { OpenProcess(PROCESS_ALL_ACCESS, false, std::process::id()) }.unwrap();
                let process = Process {
                    handle: OwnedHandle(handle),
                    pid: std::process::id(),
                };
                let region = unsafe {
                    VirtualAllocEx(
                        handle,
                        None,
                        0x1000,
                        MEM_RESERVE | MEM_COMMIT,
                        PAGE_READWRITE,
                    )
                } as usize;
                assert_ne!(region, 0);
                let pattern = Pattern::parse(recipe.pattern).unwrap();
                let bytes: Vec<u8> = pattern.0.iter().map(|b| b.unwrap_or(0x55)).collect();
                process.write(region, &bytes).unwrap();
                process.protect(region, 0x1000, PAGE_EXECUTE_READ).unwrap();
                let mut session = Session {
                    process,
                    sections: vec![(region, 0x1000)],
                    patches: BTreeMap::new(),
                    money: None,
                    hooks: BTreeMap::new(),
                    applied: BTreeMap::new(),
                    failure_requires_cleanup: false,
                    active: BTreeMap::new(),
                    cancel: Arc::new(AtomicU64::new(1)),
                    epoch: 1,
                };
                session.set(id, None, true).unwrap();
                assert_eq!(
                    session
                        .process
                        .read(region + recipe.offset, recipe.replacement.len())
                        .unwrap(),
                    recipe.replacement
                );
                session.set(id, None, false).unwrap();
                assert_eq!(session.process.read(region, bytes.len()).unwrap(), bytes);
                session
                    .process
                    .protect(region, 0x1000, PAGE_READWRITE)
                    .unwrap();
                session.process.write(region + 128, &bytes).unwrap();
                session
                    .process
                    .protect(region, 0x1000, PAGE_EXECUTE_READ)
                    .unwrap();
                assert!(session.set(id, None, true).is_err());
                assert!(session.active.is_empty());
                drop(session);
                unsafe { VirtualFreeEx(GetCurrentProcess(), region as _, 0, MEM_RELEASE) }.unwrap();
            }
        }
    }
    #[test]
    fn native_patch_and_shared_numeric_hook_round_trip() {
        let handle = unsafe { OpenProcess(PROCESS_ALL_ACCESS, false, std::process::id()) }.unwrap();
        let process = Process {
            handle: OwnedHandle(handle),
            pid: std::process::id(),
        };
        let region = unsafe {
            VirtualAllocEx(
                handle,
                None,
                0x1000,
                MEM_RESERVE | MEM_COMMIT,
                PAGE_READWRITE,
            )
        } as usize;
        assert_ne!(region, 0);
        // A real, harmless x64 test function: preserve rbx, read the supplied local
        // record, execute the exact instruction our money adapter replaces, return.
        let code = [
            0x53, 0x48, 0x89, 0xcb, 0xf2, 0x0f, 0x58, 0x4b, 0x50, 0x5b, 0xc3,
        ];
        process.write(region, &code).unwrap();
        process.protect(region, 0x1000, PAGE_EXECUTE_READ).unwrap();
        let cave = process.allocate_near(region).unwrap();
        process
            .write(cave, &money_code(cave, region + 9).unwrap())
            .unwrap();
        process.protect(cave, 0x1000, PAGE_EXECUTE_READ).unwrap();
        process.flush(cave, 0x1000).unwrap();
        let patch = Patch {
            address: region + 4,
            original: code[4..9].to_vec(),
            replacement: relative_jump(region + 4, cave).unwrap(),
        };
        let mut session = Session {
            process,
            sections: vec![],
            patches: BTreeMap::new(),
            money: Some(MoneyHook {
                patch,
                cave,
                installed: false,
            }),
            active: BTreeMap::new(),
            cancel: Arc::new(AtomicU64::new(1)),
            hooks: BTreeMap::new(),
            applied: BTreeMap::new(),
            failure_requires_cleanup: false,
            epoch: 1,
        };
        let function: unsafe extern "system" fn(*mut u8) = unsafe { std::mem::transmute(region) };
        let mut record = [0u8; 128];
        session.set("money", Some(12345.0), true).unwrap();
        session.set("agent_points", Some(678.0), true).unwrap();
        unsafe {
            function(record.as_mut_ptr());
        }
        assert_eq!(
            i32::from_le_bytes(record[0x58..0x5c].try_into().unwrap()),
            12345
        );
        assert_eq!(
            i32::from_le_bytes(record[0x5c..0x60].try_into().unwrap()),
            678
        );
        session.set("money", None, false).unwrap();
        record[0x58..0x60].fill(0);
        unsafe {
            function(record.as_mut_ptr());
        }
        assert_eq!(&record[0x58..0x5c], &[0; 4]);
        assert_eq!(
            i32::from_le_bytes(record[0x5c..0x60].try_into().unwrap()),
            678
        );
        session.cleanup().unwrap();
        assert_eq!(session.process.read(region, code.len()).unwrap(), code);
        assert!(session.active.is_empty());
        // Conflict must preserve third-party bytes, never blindly restore.
        assert!(
            session
                .process
                .replace_code(region, &[0; 4], &[0x90; 4], &[])
                .is_err()
        );
        session.cancel.store(2, Ordering::Release);
        assert!(session.set("money", Some(10.0), true).is_err());
        drop(session);
        unsafe { VirtualFreeEx(GetCurrentProcess(), region as _, 0, MEM_RELEASE) }.unwrap();
        unsafe { VirtualFreeEx(GetCurrentProcess(), cave as _, 0, MEM_RELEASE) }.unwrap();
    }

    #[test]
    fn remote_fixture() {
        let Some(path) = std::env::var_os("SLACKINPUT_TRAINER_FIXTURE") else {
            return;
        };
        let handle = unsafe { GetCurrentProcess() };
        let region = unsafe {
            VirtualAllocEx(
                handle,
                None,
                0x1000,
                MEM_RESERVE | MEM_COMMIT,
                PAGE_READWRITE,
            )
        };
        let code = [
            0x53u8, 0x48, 0x89, 0xcb, 0xf2, 0x0f, 0x58, 0x4b, 0x50, 0x5b, 0xc3,
        ];
        unsafe {
            std::ptr::copy_nonoverlapping(code.as_ptr(), region.cast(), code.len());
        }
        let mut old = PAGE_PROTECTION_FLAGS::default();
        unsafe { VirtualProtectEx(handle, region, 0x1000, PAGE_EXECUTE_READ, &mut old) }.unwrap();
        unsafe { FlushInstructionCache(handle, Some(region), 0x1000) }.unwrap();
        let mut record = Box::new([0u8; 128]);
        let data = (
            std::process::id(),
            creation_time(handle).unwrap(),
            region as usize,
            record.as_mut_ptr() as usize,
        );
        std::fs::write(path, serde_json::to_vec(&data).unwrap()).unwrap();
        let function: unsafe extern "system" fn(*mut u8) = unsafe { std::mem::transmute(region) };
        loop {
            unsafe {
                function(record.as_mut_ptr());
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn remote_numeric_hook_suspends_verifies_and_restores_a_test_process() {
        use std::{
            os::windows::process::CommandExt,
            process::{Command, Stdio},
            time::{Duration, Instant},
        };
        let path = std::env::temp_dir().join(format!(
            "slackinput-trainer-fixture-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "trainer::native::tests::remote_fixture",
                "--nocapture",
            ])
            .env("SLACKINPUT_TRAINER_FIXTURE", &path)
            .creation_flags(0x08000000)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        struct Fixture(std::process::Child, std::path::PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
                let _ = std::fs::remove_file(&self.1);
            }
        }
        let _fixture = Fixture(child, path.clone());
        let deadline = Instant::now() + Duration::from_secs(10);
        let (pid, created, region, record): (u32, u64, usize, usize) = loop {
            if let Ok(bytes) = std::fs::read(&path) {
                if let Ok(data) = serde_json::from_slice(&bytes) {
                    break data;
                }
            }
            assert!(Instant::now() < deadline, "fixture startup timed out");
            std::thread::sleep(Duration::from_millis(10));
        };
        let target = Target {
            pid,
            created,
            generation: 1,
            name: "test.exe".into(),
        };
        let process = Process::open(&target).unwrap();
        let cave = process.allocate_near(region).unwrap();
        let code = money_code(cave, region + 9).unwrap();
        process.write(cave, &code).unwrap();
        process.protect(cave, 0x1000, PAGE_EXECUTE_READ).unwrap();
        process.flush(cave, code.len()).unwrap();
        let original = vec![0xf2, 0x0f, 0x58, 0x4b, 0x50];
        let patch = Patch {
            address: region + 4,
            original: original.clone(),
            replacement: relative_jump(region + 4, cave).unwrap(),
        };
        let mut session = Session {
            process,
            sections: vec![(region, 11)],
            patches: BTreeMap::new(),
            money: Some(MoneyHook {
                patch,
                cave,
                installed: false,
            }),
            active: BTreeMap::new(),
            cancel: Arc::new(AtomicU64::new(1)),
            hooks: BTreeMap::new(),
            applied: BTreeMap::new(),
            failure_requires_cleanup: false,
            epoch: 1,
        };
        session.set("money", Some(23456.0), true).unwrap();
        session.set("agent_points", Some(321.0), true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let actual = session.process.read(record + 0x58, 8).unwrap();
            if actual[..4] == 23456i32.to_le_bytes() && actual[4..] == 321i32.to_le_bytes() {
                break;
            }
            assert!(Instant::now() < deadline, "remote test hook did not run");
            std::thread::sleep(Duration::from_millis(10));
        }
        session.cleanup().unwrap();
        assert_eq!(session.process.read(region + 4, 5).unwrap(), original);
        assert!(session.active.is_empty());
    }
}
