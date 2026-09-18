//! Executes only generated adapter code against memory owned by this test process.
use super::*;
use iced_x86::code_asm::*;

fn local_process() -> Process {
    Process {
        handle: OwnedHandle(
            unsafe { OpenProcess(PROCESS_ALL_ACCESS, false, std::process::id()) }.unwrap(),
        ),
        pid: std::process::id(),
    }
}

fn original(group: Group, variant: usize) -> Vec<u8> {
    let mut bytes: Vec<_> = Pattern::parse(group.variants()[variant].original)
        .unwrap()
        .0
        .into_iter()
        .map(|v| v.unwrap_or(0))
        .collect();
    match group {
        Group::Battle => bytes[2] = 0x40,
        Group::Damage if variant == 0 => {
            bytes[4] = 0x48;
            bytes[6] = 0xf2;
        }
        Group::CardRewards | Group::CardWin => bytes[2] = 0x40,
        _ => {}
    }
    bytes
}

struct Fixture {
    process: Process,
    cave: usize,
}
impl Fixture {
    fn new(group: Group, variant: usize) -> Self {
        let process = local_process();
        let cave = unsafe {
            VirtualAllocEx(
                process.handle.0,
                None,
                0x2000,
                MEM_RESERVE | MEM_COMMIT,
                PAGE_READWRITE,
            )
        } as usize;
        assert_ne!(cave, 0);
        let body = hooks::build(group, &original(group, variant), cave, cave + 0x800).unwrap();
        assert!(body.len() < 0x800);
        process.write(cave, &body).unwrap();
        process.write(cave + 0x800, &[0xc3]).unwrap();
        // A Windows x64 ABI wrapper. The original game instructions may modify
        // nonvolatile registers, so keep the context pointer on our own stack.
        let mut a = CodeAssembler::new(64).unwrap();
        for reg in [rbx, rsi, rdi, r12, r13, r14, r15] {
            a.push(reg).unwrap();
        }
        a.sub(rsp, 80).unwrap();
        a.movdqu(xmmword_ptr(rsp + 32), xmm6).unwrap();
        a.movdqu(xmmword_ptr(rsp + 48), xmm7).unwrap();
        a.mov(qword_ptr(rsp + 64), rcx).unwrap();
        a.mov(r12, rcx).unwrap();
        for (index, reg) in [rax, rcx, rdx, rbx, rsi, rdi, r13, r14, r15, r11]
            .into_iter()
            .enumerate()
        {
            a.mov(reg, qword_ptr(r12 + index as i32 * 8)).unwrap();
        }
        a.movss(xmm6, dword_ptr(r12 + 80)).unwrap();
        a.call(cave as u64).unwrap();
        a.mov(r12, qword_ptr(rsp + 64)).unwrap();
        for (index, reg) in [rax, rcx, rdx, rbx, rsi, rdi, r13, r14, r15, r11]
            .into_iter()
            .enumerate()
        {
            a.mov(qword_ptr(r12 + index as i32 * 8), reg).unwrap();
        }
        a.movss(dword_ptr(r12 + 80), xmm6).unwrap();
        a.pushfq().unwrap();
        a.pop(qword_ptr(r12 + 88)).unwrap();
        a.movdqu(xmm6, xmmword_ptr(rsp + 32)).unwrap();
        a.movdqu(xmm7, xmmword_ptr(rsp + 48)).unwrap();
        a.add(rsp, 80).unwrap();
        for reg in [r15, r14, r13, r12, rdi, rsi, rbx] {
            a.pop(reg).unwrap();
        }
        a.ret().unwrap();
        let wrapper = a.assemble((cave + 0x900) as u64).unwrap();
        assert!(wrapper.len() < 0x700);
        process.write(cave + 0x900, &wrapper).unwrap();
        process
            .write(cave + 0x1104, &9999999f32.to_le_bytes())
            .unwrap();
        process.protect(cave, 0x1000, PAGE_EXECUTE_READ).unwrap();
        process.flush(cave, 0x1000).unwrap();
        Self { process, cave }
    }
    fn parameter(&self, index: usize, bits: u32) {
        self.process
            .write(self.cave + 0x1000 + index * 4, &bits.to_le_bytes())
            .unwrap();
    }
    fn run(&self, registers: &mut [u64; 12]) {
        registers[9] = 0x123456789abcdef;
        let call: unsafe extern "system" fn(*mut u64) =
            unsafe { std::mem::transmute(self.cave + 0x900) };
        unsafe {
            call(registers.as_mut_ptr());
        }
        assert_eq!(registers[9], 0x123456789abcdef, "scratch register restored");
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        unsafe { VirtualFreeEx(self.process.handle.0, self.cave as _, 0, MEM_RELEASE) }.unwrap();
    }
}
fn put(record: &mut [u8], offset: usize, value: i32) {
    record[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
fn get(record: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes(record[offset..offset + 4].try_into().unwrap())
}

#[test]
fn battle_cp_scan_execute_all_variants() {
    let f = Fixture::new(Group::Battle, 0);
    let mut data = [0u8; 512];
    let mut r = [0; 12];
    r[5] = data.as_mut_ptr() as u64;
    put(&mut data, 0x3c, 1);
    put(&mut data, 0x40, 1);
    f.parameter(0, 1);
    f.run(&mut r);
    assert_eq!(get(&data, 0x48), 99999);
    assert_eq!(get(&data, 0x50), 0);
    assert_ne!(r[11] & 0x40, 0);
    f.parameter(0, 0);
    f.parameter(1, 1);
    put(&mut data, 0x48, 20);
    f.run(&mut r);
    assert_eq!(get(&data, 0x48), 20);
    assert_eq!(get(&data, 0x50), 99999);
    put(&mut data, 0x3c, 2);
    put(&mut data, 0x50, 30);
    f.run(&mut r);
    assert_eq!(get(&data, 0x50), 30);
    for variant in 0..2 {
        let f = Fixture::new(Group::Cp, variant);
        let mut r = [0; 12];
        r[1] = data.as_mut_ptr() as u64;
        r[6] = r[1];
        put(&mut data, 0x1d0, 123);
        data[0x1a0] = 7;
        f.run(&mut r);
        assert_eq!(get(&data, 0x1a4), 123);
        assert_eq!(r[0], 7);
        let f = Fixture::new(Group::Scan, variant);
        r[3] = data.as_mut_ptr() as u64;
        put(&mut data, 8, 10);
        f.run(&mut r);
        assert_eq!(get(&data, 8), 9999);
        put(&mut data, 8, 0);
        f.run(&mut r);
        assert_eq!(get(&data, 8), 0);
    }
}

#[test]
fn damage_and_rewards_use_fractional_independent_parameters() {
    let mut target = [0u8; 512];
    let mut damage = [0u8; 64];
    for variant in 0..2 {
        let f = Fixture::new(Group::Damage, variant);
        let mut r = [0; 12];
        r[1] = target.as_mut_ptr() as u64;
        r[2] = damage.as_mut_ptr() as u64;
        f.parameter(1, 2.5f32.to_bits());
        f.parameter(2, 4f32.to_bits());
        put(&mut target, 12, 2);
        put(&mut damage, 12, 40);
        f.run(&mut r);
        assert_eq!(get(&damage, 12), 100);
        put(&mut target, 12, 1);
        put(&mut damage, 12, 40);
        f.run(&mut r);
        assert_eq!(get(&damage, 12), 10);
        put(&mut target, 12, 2);
        f.parameter(0, 1);
        f.run(&mut r);
        assert_eq!(get(&damage, 12), 99999);
        f.parameter(0, 0);
        put(&mut damage, 12, i32::MAX);
        f.run(&mut r);
        assert_eq!(get(&damage, 12), i32::MAX);
    }
    for variant in 0..2 {
        let f = Fixture::new(Group::Rewards, variant);
        let mut r = [0; 12];
        r[8] = target.as_mut_ptr() as u64;
        f.parameter(0, 1.5f32.to_bits());
        f.parameter(2, 2.5f32.to_bits());
        put(&mut target, 0x160, 20);
        put(&mut target, 0x15c, 40);
        f.run(&mut r);
        assert_eq!(get(&target, 0x160), 30);
        assert_eq!(get(&target, 0x15c), 100);
        f.parameter(0, 0);
        f.parameter(1, 1);
        f.run(&mut r);
        assert_eq!(get(&target, 0x160), 30);
        assert_eq!(get(&target, 0x15c), 9999999);
    }
}

#[test]
fn all_seven_item_categories_execute_without_cross_writes() {
    let f = Fixture::new(Group::Items, 0);
    let mut item = [0u8; 32];
    let mut category = [0u8; 256];
    let mut inventory = [0u8; 32];
    for i in 0..7 {
        f.parameter(i, 100 + i as u32);
    }
    for (kind, subcategory, expected) in [
        (3, 0, 100),
        (3, 1, 101),
        (3, 2, 102),
        (3, 3, 103),
        (3, 4, 104),
        (3, 5, 105),
        (1, 0, 106),
        (2, 0, 103),
        (3, 7, 17),
        (4, 0, 17),
    ] {
        put(&mut item, 12, 17);
        put(&mut item, 16, 3);
        put(&mut category, 0xe0, kind);
        put(&mut inventory, 12, subcategory);
        let mut r = [0; 12];
        r[0] = item.as_mut_ptr() as u64;
        r[7] = category.as_mut_ptr() as u64;
        r[8] = inventory.as_mut_ptr() as u64;
        f.run(&mut r);
        assert_eq!(get(&item, 12), expected);
        assert_eq!(r[2], (expected - 3) as u64);
    }
}

#[test]
fn bond_evolution_cards_and_stats_capture_execute() {
    let f = Fixture::new(Group::Bond, 0);
    let mut r = [0; 12];
    f.parameter(1, 2.5f32.to_bits());
    r[10] = 4f32.to_bits() as u64;
    f.run(&mut r);
    assert_eq!(f32::from_bits(r[10] as u32), 10.0);
    f.parameter(0, 1);
    f.run(&mut r);
    assert_eq!(f32::from_bits(r[10] as u32), 9999999.0);
    r[10] = (-2f32).to_bits() as u64;
    f.run(&mut r);
    assert_eq!(f32::from_bits(r[10] as u32), -2.0);
    let f = Fixture::new(Group::Evolution, 0);
    let mut data = [0x5au8; 512];
    r[2] = data.as_mut_ptr() as u64;
    f.run(&mut r);
    for offset in (4..=0x3c).step_by(4).chain([0x50]) {
        assert_eq!(get(&data, offset), 0);
    }
    assert_eq!(data[0x40], 0x5a);
    let f = Fixture::new(Group::CardRewards, 0);
    r[3] = data.as_mut_ptr() as u64;
    r[0] = 2;
    f.run(&mut r);
    assert_eq!(r[0], 99);
    assert_eq!(r[11] & (0x40 | 0x80), 0);
    r[0] = 0;
    put(&mut data, 0x40, 0);
    f.run(&mut r);
    assert_eq!(r[0], 0);
    assert_ne!(r[11] & 0x40, 0);
    let f = Fixture::new(Group::CardWin, 0);
    r[0] = 123;
    f.run(&mut r);
    assert_eq!(get(&data, 0x40), 0);
    let f = Fixture::new(Group::Stats, 0);
    put(&mut data, 4, 1234);
    put(&mut data, 0x78, 77);
    r[5] = data.as_mut_ptr() as u64;
    f.run(&mut r);
    assert_eq!(
        f.process.read(f.cave + 0x1000, 8).unwrap(),
        r[5].to_le_bytes()
    );
    assert_eq!(
        f.process.read(f.cave + 0x1008, 4).unwrap(),
        1234u32.to_le_bytes()
    );
    assert_eq!(r[0], 77);
    assert_eq!(r[2], 1);
}

fn synthetic_session(group: Group, variant: usize) -> (Session, usize, Vec<u8>) {
    let process = local_process();
    let region = unsafe {
        VirtualAllocEx(
            process.handle.0,
            None,
            0x1000,
            MEM_RESERVE | MEM_COMMIT,
            PAGE_READWRITE,
        )
    } as usize;
    assert_ne!(region, 0);
    let recipe = &group.variants()[variant];
    let mut bytes: Vec<_> = Pattern::parse(recipe.pattern)
        .unwrap()
        .0
        .into_iter()
        .map(|v| v.unwrap_or(0x55))
        .collect();
    let instructions = original(group, variant);
    bytes[recipe.offset..recipe.offset + instructions.len()].copy_from_slice(&instructions);
    process.write(region, &bytes).unwrap();
    if let Some(layout) = group.layout_pattern() {
        let layout: Vec<_> = Pattern::parse(layout)
            .unwrap()
            .0
            .into_iter()
            .map(|v| v.unwrap_or(0x55))
            .collect();
        process.write(region + 0x200, &layout).unwrap();
    }
    process.protect(region, 0x1000, PAGE_EXECUTE_READ).unwrap();
    (
        Session {
            process,
            sections: vec![(region, 0x1000)],
            patches: BTreeMap::new(),
            money: None,
            hooks: BTreeMap::new(),
            active: BTreeMap::new(),
            applied: BTreeMap::new(),
            failure_requires_cleanup: false,
            cancel: Arc::new(AtomicU64::new(1)),
            epoch: 1,
        },
        region,
        bytes,
    )
}

#[test]
fn every_group_scans_installs_shares_parameters_and_restores() {
    let catalog = profile::builtin();
    for group in [
        Group::Battle,
        Group::Cp,
        Group::Scan,
        Group::Damage,
        Group::Items,
        Group::Rewards,
        Group::Evolution,
        Group::Bond,
        Group::CardRewards,
        Group::CardWin,
    ] {
        for variant in 0..group.variants().len() {
            let (mut session, region, bytes) = synthetic_session(group, variant);
            for (slot, id) in group.parameters().iter().enumerate() {
                let feature = catalog.features.iter().find(|f| f.id == *id).unwrap();
                let value = feature.parse_value(&feature.default_text()).unwrap();
                session
                    .set(id, value, true)
                    .unwrap_or_else(|e| panic!("{group:?}/{variant}/{id}: {e}"));
                let expected = if id.ends_with("_f") {
                    (value.unwrap() as f32).to_bits()
                } else {
                    value.unwrap_or(1.0) as u32
                };
                assert_eq!(
                    session
                        .process
                        .read(session.hooks[&group].cave + 0x1000 + slot * 4, 4)
                        .unwrap(),
                    expected.to_le_bytes()
                );
            }
            let cave = session.hooks[&group].cave;
            for (slot, id) in group.parameters().iter().enumerate() {
                session.set(id, None, false).unwrap();
                assert_eq!(
                    session.hooks[&group].installed,
                    slot + 1 < group.parameters().len()
                );
                assert_eq!(
                    session.process.read(cave + 0x1000 + slot * 4, 4).unwrap(),
                    [0; 4]
                );
            }
            assert!(session.active.is_empty());
            assert_eq!(session.process.read(region, bytes.len()).unwrap(), bytes);
            // Re-enable a cached hook, then exercise full-session cleanup.
            let id = group.parameters()[0];
            let feature = catalog.features.iter().find(|f| f.id == id).unwrap();
            session
                .set(
                    id,
                    feature.parse_value(&feature.default_text()).unwrap(),
                    true,
                )
                .unwrap();
            session.cleanup().unwrap();
            assert!(session.active.is_empty());
            assert!(!session.needs_cleanup());
            assert_eq!(session.process.read(region, bytes.len()).unwrap(), bytes);
            drop(session);
            unsafe { VirtualFreeEx(GetCurrentProcess(), region as _, 0, MEM_RELEASE) }.unwrap();
            unsafe { VirtualFreeEx(GetCurrentProcess(), cave as _, 0, MEM_RELEASE) }.unwrap();
        }
    }
}

#[test]
fn all_thirteen_stat_writes_require_valid_capture_and_apply_once() {
    let (mut session, region, bytes) = synthetic_session(Group::Stats, 0);
    let error = session
        .set("e_digimon_level", Some(55.0), true)
        .unwrap_err();
    assert!(error.contains("属性界面"));
    assert!(!session.needs_cleanup());
    let cave = session.hooks[&Group::Stats].cave;
    let mut data = [0u8; 512];
    put(&mut data, 4, 1234);
    session
        .process
        .write(cave + 0x1000, &(data.as_mut_ptr() as u64).to_le_bytes())
        .unwrap();
    session
        .process
        .write(cave + 0x1008, &1234u32.to_le_bytes())
        .unwrap();
    for (id, _) in hooks::STATS {
        session.set(id, Some(55.0), true).unwrap();
        assert_eq!(session.applied[*id], (55.0, 1234));
        for (offset, expected) in hooks::stat_writes(id, 55).unwrap() {
            assert_eq!(&data[offset..offset + 4], &expected);
        }
    }
    assert_eq!(get(&data, 0x104), 55000);
    assert_eq!(get(&data, 0x108), 55000);
    assert_eq!(f32::from_bits(get(&data, 0x144) as u32), 5500.0);
    assert!(session.active.is_empty());
    put(&mut data, 4, 5678);
    let before = data;
    assert!(
        session
            .set("e_digimon_level", Some(77.0), true)
            .unwrap_err()
            .contains("对象已变化")
    );
    assert_eq!(data, before);
    assert!(
        session
            .set("e_digimon_talent", Some(i32::MAX as f64), true)
            .is_err()
    );
    session.cleanup().unwrap();
    assert_eq!(session.process.read(region, bytes.len()).unwrap(), bytes);
    // A subsequent session must request a fresh selection, not reuse a stale pointer.
    assert!(
        session
            .set("e_digimon_level", Some(77.0), true)
            .unwrap_err()
            .contains("属性界面")
    );
    session.cleanup().unwrap();
    drop(session);
    unsafe { VirtualFreeEx(GetCurrentProcess(), region as _, 0, MEM_RELEASE) }.unwrap();
    unsafe { VirtualFreeEx(GetCurrentProcess(), cave as _, 0, MEM_RELEASE) }.unwrap();
}
