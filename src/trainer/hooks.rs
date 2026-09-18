//! Explicit x64 recipes reconstructed from the supplied sample. No imported code is executed.
use iced_x86::{Decoder, DecoderOptions, FlowControl, OpKind, code_asm::*};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Group { Battle, Cp, Scan, Damage, Items, Rewards, Evolution, Bond, CardRewards, CardWin, Stats }

pub struct Variant {
    pub pattern: &'static str,
    pub offset: usize,
    pub original: &'static str,
}

pub const STATS: &[(&str, usize)] = &[
    ("e_digimon_level", 0x68), ("e_digimon_exp", 0x6c),
    ("e_digimon_current_hp", 0x74), ("e_digimon_current_sp", 0x78),
    ("e_digimon_max_hp_bonus", 0xb4), ("e_digimon_max_sp_bonus", 0xb8),
    ("e_digimon_atk_bonus", 0xbc), ("e_digimon_def_bonus", 0xc0),
    ("e_digimon_int_bonus", 0xc4), ("e_digimon_spi_bonus", 0xc8),
    ("e_digimon_spd_bonus", 0xcc), ("e_digimon_talent", 0x104), ("e_digimon_bond", 0x144),
];

pub fn group(id: &str) -> Option<Group> {
    Some(match id {
        "inf_hp" | "inf_sp" => Group::Battle,
        "infinite_cp" => Group::Cp,
        "max_scan_rate" => Group::Scan,
        "dmg" | "dmgmul_f" | "defmul_f" => Group::Damage,
        "recovery" | "enhance" | "special" | "equip" | "material" | "farm" | "skill" => Group::Items,
        "moneymul_f" | "exp" | "expmul_f" => Group::Rewards,
        "no_digivolution_requirements" => Group::Evolution,
        "bond" | "bondmul_f" => Group::Bond,
        "card_game_max_reward_selection" => Group::CardRewards,
        "card_game_always_win" => Group::CardWin,
        _ if STATS.iter().any(|(name, _)| *name == id) => Group::Stats,
        _ => return None,
    })
}

impl Group {
    pub fn parameters(self) -> &'static [&'static str] {
        match self {
            Self::Battle => &["inf_hp", "inf_sp"],
            Self::Damage => &["dmg", "dmgmul_f", "defmul_f"],
            Self::Items => &["recovery", "enhance", "special", "equip", "material", "farm", "skill"],
            Self::Rewards => &["moneymul_f", "exp", "expmul_f"],
            Self::Bond => &["bond", "bondmul_f"],
            Self::Cp => &["infinite_cp"], Self::Scan => &["max_scan_rate"],
            Self::Evolution => &["no_digivolution_requirements"],
            Self::CardRewards => &["card_game_max_reward_selection"],
            Self::CardWin => &["card_game_always_win"], Self::Stats => &[],
        }
    }
    pub fn variants(self) -> Vec<Variant> {
        match self {
            Self::Battle => vec![Variant { pattern: "83 BF * * 00 00 01 0F 85 * * 00 00 * 8D * * * 00 00 E8", offset: 0, original: "83 BF * * 00 00 01" }],
            Self::Cp => vec![
                Variant { pattern: "0F B6 81 A0 01 00 00 88 * 8B 81 A4 01 00 00 89 * 04 8B", offset: 0, original: "0F B6 81 A0 01 00 00" },
                Variant { pattern: "41 0F B6 85 A0 01 00 00 88 * * 8B * * * 00 00 89", offset: 0, original: "41 0F B6 85 A0 01 00 00" },
            ],
            Self::Scan => vec![
                Variant { pattern: "44 8B 43 08 48 8B CE * * E8 * * * * * 83 * * * * * 75", offset: 0, original: "44 8B 43 08 48 8B CE" },
                Variant { pattern: "44 8B 43 08 8B 13 * * * E8 * * * * * 83 * * * * * 75", offset: 0, original: "44 8B 43 08 8B 13" },
            ],
            Self::Damage => vec![
                Variant { pattern: "80 7A 24 00 * 8B * * 8B * 0F 85 * * 00 00", offset: 0, original: "80 7A 24 00 * 8B *" },
                Variant { pattern: "80 7A 24 00 48 89 D6 49 89 CF 0F 85 * * 00 00", offset: 0, original: "80 7A 24 00 48 89 D6" },
            ],
            Self::Items => vec![Variant { pattern: "41 83 7F 0C 07 74 * * * * * * * * E8 * * * * 8B 50 0C 2B 50 10 * * * E8 * * * * 41 83 BE E0 00 00 00 04 74", offset: 0x13, original: "8B 50 0C 2B 50 10" }],
            Self::Rewards => vec![
                Variant { pattern: "49 8D 7F 40 44 8B E0 * 8B", offset: 0, original: "49 8D 7F 40 44 8B E0" },
                Variant { pattern: "44 8B E8 49 8D 77 40", offset: 0, original: "44 8B E8 49 8D 77 40" },
            ],
            Self::Evolution => vec![Variant { pattern: "0F 85 * * 00 00 8B 42 04 85 C0 74 * * 88", offset: 6, original: "8B 42 04 85 C0" }],
            Self::Bond => vec![Variant { pattern: "0F 57 FF 0F 2E F7 7A * 0F 84 * * 00 00 E8 * * * * * * * * 85 * 75", offset: 0, original: "0F 57 FF 0F 2E F7" }],
            Self::CardRewards => vec![Variant { pattern: "39 83 * * 00 00 7E * 89 * * * 00 00 E8 * * * * * * * E8 * * * * * 8D", offset: 0, original: "39 83 * * 00 00" }],
            Self::CardWin => vec![Variant { pattern: "48 8B * * * 00 00 E8 * * * * * * * * * * * * * * * * * E8 * * * * 89 83 * * 00 00 E8 * * * * * * * E8 * * * * * 8D * * * 00 00", offset: 0x1e, original: "89 83 * * 00 00" }],
            Self::Stats => vec![Variant { pattern: "89 * 24 * E8 * * * * 8B 47 78 BA 01 00 00 00", offset: 9, original: "8B 47 78 BA 01 00 00 00" }],
        }
    }
    /// Independent layout evidence from the original adapter's secondary scan.
    pub fn layout_pattern(self) -> Option<&'static str> {
        match self {
            Self::Rewards => Some("E8 * * * * 41 01 * 60 01 00 00 * 8B * E8"),
            Self::Stats => Some("F3 0F 10 * 44 01 00 00 66 0F 6E * 0F 5B * F3 0F 5E"),
            _ => None,
        }
    }
}

/// Verify entire instructions before relocating their bytes. These recipes never
/// contain RIP-relative operands or branches in the overwritten span.
pub fn validate_original(group: Group, bytes: &[u8]) -> Result<(), String> {
    if bytes.len() < 5 || bytes.len() > 16 { return Err("目标指令长度无效".into()); }
    if group == Group::Battle && (bytes.len() != 7 || i32::from_le_bytes(bytes[2..6].try_into().unwrap()) < 4) {
        return Err("战斗字段偏移无效".into());
    }
    let mut decoder = Decoder::with_ip(64, bytes, 0x10000, DecoderOptions::NONE);
    let mut instructions = vec![];
    while decoder.can_decode() {
        let i = decoder.decode();
        if i.is_invalid() || i.flow_control() != FlowControl::Next || i.is_ip_rel_memory_operand() {
            return Err("目标指令不能由此适配器安全重定位".into());
        }
        instructions.push(i);
    }
    if group == Group::Damage {
        let i = instructions.last().ok_or("缺少伤害指令")?;
        if i.op0_kind() != OpKind::Register || i.op1_kind() != OpKind::Register {
            return Err("伤害脚本寄存器变体不匹配".into());
        }
    }
    Ok(())
}

/// Preserve registers/flags not changed by the overwritten instructions. Parameters
/// are in a separate RW page; all generated code is RX after installation.
pub fn build(group: Group, original: &[u8], cave: usize, return_address: usize) -> Result<Vec<u8>, String> {
    validate_original(group, original)?;
    assemble(group, original, cave as u64, return_address as u64).map_err(|e| e.to_string())
}

fn scaled_integer(a: &mut CodeAssembler, field: AsmMemoryOperand, parameter: AsmMemoryOperand, divide: bool) -> Result<(), iced_x86::IcedError> {
    let mut skip = a.create_label(); let mut upper_ok = a.create_label(); let mut lower_ok = a.create_label();
    a.cmp(parameter, 0)?; a.jle(skip)?;
    a.push(rax)?;
    a.fild(field)?;
    if divide { a.fdiv(parameter)?; } else { a.fmul(parameter)?; }
    a.sub(rsp, 8)?; a.fistp(qword_ptr(rsp))?; a.mov(rax, qword_ptr(rsp))?; a.add(rsp, 8)?;
    // Avoid the original's i32 overflow for very large reward/damage multipliers.
    a.cmp(rax, i32::MAX)?; a.jle(upper_ok)?; a.mov(rax, i32::MAX as i64)?;
    a.set_label(&mut upper_ok)?;
    a.cmp(rax, i32::MIN)?; a.jge(lower_ok)?; a.mov(rax, i32::MIN as i64)?;
    a.set_label(&mut lower_ok)?;
    a.mov(field, eax)?; a.pop(rax)?; a.set_label(&mut skip)?;
    Ok(())
}

fn assemble(group: Group, original: &[u8], cave: u64, return_address: u64) -> Result<Vec<u8>, iced_x86::IcedError> {
    let mut a = CodeAssembler::new(64)?;
    a.pushfq()?; a.push(r11)?; a.mov(r11, cave + 0x1000)?;
    let mut done = a.create_label();
    match group {
        Group::Battle => {
            let displacement = i32::from_le_bytes(original[2..6].try_into().unwrap());
            a.cmp(dword_ptr(rdi + displacement - 4), 1)?; a.jne(done)?;
            let mut sp_label = a.create_label();
            a.cmp(dword_ptr(r11), 1)?; a.jne(sp_label)?; a.mov(dword_ptr(rdi + displacement + 8), 99999)?;
            a.set_label(&mut sp_label)?;
            a.cmp(dword_ptr(r11 + 4), 1)?; a.jne(done)?; a.mov(dword_ptr(rdi + displacement + 0x10), 99999)?;
        }
        Group::Cp => {
            a.push(rax)?;
            if original[0] == 0x41 { a.mov(eax, dword_ptr(r13 + 0x1d0))?; a.mov(dword_ptr(r13 + 0x1a4), eax)?; }
            else { a.mov(eax, dword_ptr(rcx + 0x1d0))?; a.mov(dword_ptr(rcx + 0x1a4), eax)?; }
            a.pop(rax)?;
        }
        Group::Scan => {
            a.cmp(dword_ptr(rbx + 8), 0)?; a.jle(done)?; a.mov(dword_ptr(rbx + 8), 9999)?;
        }
        Group::Damage => {
            let mut enemy = a.create_label();
            a.test(rcx, rcx)?; a.je(done)?; a.cmp(dword_ptr(rcx + 0xc), 2)?; a.je(enemy)?;
            scaled_integer(&mut a, dword_ptr(rdx + 0xc), dword_ptr(r11 + 8), true)?; a.jmp(done)?;
            a.set_label(&mut enemy)?;
            scaled_integer(&mut a, dword_ptr(rdx + 0xc), dword_ptr(r11 + 4), false)?;
            a.cmp(dword_ptr(r11), 1)?; a.jne(done)?; a.mov(dword_ptr(rdx + 0xc), 99999)?;
        }
        Group::Items => {
            let mut other = a.create_label(); let mut set_amount = a.create_label(); let mut end_items = a.create_label();
            a.push(rbx)?; a.mov(ebx, dword_ptr(r14 + 0xe0))?; a.cmp(ebx, 3)?; a.jne(other)?;
            a.mov(edx, dword_ptr(r15 + 0xc))?;
            for index in 0..6 {
                let mut next = a.create_label();
                a.cmp(edx, index)?; a.jne(next)?; a.mov(edx, dword_ptr(r11 + index * 4))?; a.jmp(set_amount)?; a.set_label(&mut next)?;
            }
            a.jmp(end_items)?; a.set_label(&mut other)?;
            let mut equipment = a.create_label();
            a.cmp(ebx, 1)?; a.jne(equipment)?; a.mov(edx, dword_ptr(r11 + 24))?; a.jmp(set_amount)?;
            a.set_label(&mut equipment)?; a.cmp(ebx, 2)?; a.jne(end_items)?; a.mov(edx, dword_ptr(r11 + 12))?;
            a.set_label(&mut set_amount)?;
            a.test(edx, edx)?; a.jle(end_items)?; a.mov(dword_ptr(rax + 0xc), edx)?;
            a.set_label(&mut end_items)?; a.pop(rbx)?;
        }
        Group::Rewards => {
            scaled_integer(&mut a, dword_ptr(r15 + 0x160), dword_ptr(r11), false)?;
            scaled_integer(&mut a, dword_ptr(r15 + 0x15c), dword_ptr(r11 + 8), false)?;
            a.cmp(dword_ptr(r11 + 4), 1)?; a.jne(done)?; a.mov(dword_ptr(r15 + 0x15c), 9999999)?;
        }
        Group::Evolution => {
            for offset in (4..=0x3c).step_by(4).chain([0x50]) { a.mov(dword_ptr(rdx + offset), 0)?; }
        }
        Group::Bond => {
            a.comiss(xmm6, dword_ptr(r11 + 0x100))?; a.jbe(done)?;
            let mut maximum = a.create_label();
            a.cmp(dword_ptr(r11 + 4), 0)?; a.jle(maximum)?; a.mulss(xmm6, dword_ptr(r11 + 4))?;
            a.set_label(&mut maximum)?; a.cmp(dword_ptr(r11), 1)?; a.jne(done)?;
            a.movss(xmm6, dword_ptr(r11 + 0x104))?;
        }
        Group::CardRewards => {
            a.test(eax, eax)?; a.jle(done)?; a.mov(eax, 99)?;
            a.pop(r11)?; a.popfq()?; a.cmp(eax, 0)?; a.jmp(return_address)?;
        }
        Group::CardWin => { a.xor(eax, eax)?; }
        Group::Stats => {
            a.mov(qword_ptr(r11), rdi)?; a.mov(edx, dword_ptr(rdi + 4))?; a.mov(dword_ptr(r11 + 8), edx)?;
        }
    }
    a.set_label(&mut done)?; a.pop(r11)?; a.popfq()?;
    a.db(original)?; a.jmp(return_address)?;
    a.assemble(cave)
}

/// One-shot fields from the sample's native editor callback (RVA 0x53960).
/// Talent uses two fixed-point fields; bond uses a float scaled by 100.
pub fn stat_writes(id: &str, value: i32) -> Result<Vec<(usize, [u8;4])>, String> {
    let offset = STATS.iter().find(|(name, _)| *name == id).map(|(_, o)| *o).ok_or("未知属性")?;
    match id {
        "e_digimon_talent" => {
            let value = value.checked_mul(1000).ok_or("才能数值超出可表示范围")?;
            Ok(vec![(0x104, value.to_le_bytes()), (0x108, value.to_le_bytes())])
        }
        "e_digimon_bond" => {
            let scaled = value.checked_mul(100).ok_or("友情数值超出可表示范围")?;
            Ok(vec![(0x144, (scaled as f32).to_le_bytes())])
        }
        _ => Ok(vec![(offset, value.to_le_bytes())]),
    }
}
