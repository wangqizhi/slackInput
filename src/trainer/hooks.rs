//! Explicit x64 recipes reconstructed from the supplied sample. No imported code is executed.
use iced_x86::{Decoder, DecoderOptions, FlowControl, OpKind, code_asm::*};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Group {
    Battle,
    Cp,
    Scan,
    Damage,
    Items,
    Rewards,
    Evolution,
    Bond,
    CardRewards,
    CardWin,
    Stats,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Variant {
    pub pattern: String,
    pub offset: usize,
    pub original: String,
}

pub const STATS: &[&str] = &[
    "e_digimon_level",
    "e_digimon_exp",
    "e_digimon_current_hp",
    "e_digimon_current_sp",
    "e_digimon_max_hp_bonus",
    "e_digimon_max_sp_bonus",
    "e_digimon_atk_bonus",
    "e_digimon_def_bonus",
    "e_digimon_int_bonus",
    "e_digimon_spi_bonus",
    "e_digimon_spd_bonus",
    "e_digimon_talent",
    "e_digimon_bond",
];

pub fn group(id: &str) -> Option<Group> {
    Some(match id {
        "inf_hp" | "inf_sp" => Group::Battle,
        "infinite_cp" => Group::Cp,
        "max_scan_rate" => Group::Scan,
        "dmg" | "dmgmul_f" | "defmul_f" => Group::Damage,
        "recovery" | "enhance" | "special" | "equip" | "material" | "farm" | "skill" => {
            Group::Items
        }
        "moneymul_f" | "exp" | "expmul_f" => Group::Rewards,
        "no_digivolution_requirements" => Group::Evolution,
        "bond" | "bondmul_f" => Group::Bond,
        "card_game_max_reward_selection" => Group::CardRewards,
        "card_game_always_win" => Group::CardWin,
        _ if STATS.contains(&id) => Group::Stats,
        _ => return None,
    })
}

impl Group {
    pub fn parameters(self) -> &'static [&'static str] {
        match self {
            Self::Battle => &["inf_hp", "inf_sp"],
            Self::Damage => &["dmg", "dmgmul_f", "defmul_f"],
            Self::Items => &[
                "recovery", "enhance", "special", "equip", "material", "farm", "skill",
            ],
            Self::Rewards => &["moneymul_f", "exp", "expmul_f"],
            Self::Bond => &["bond", "bondmul_f"],
            Self::Cp => &["infinite_cp"],
            Self::Scan => &["max_scan_rate"],
            Self::Evolution => &["no_digivolution_requirements"],
            Self::CardRewards => &["card_game_max_reward_selection"],
            Self::CardWin => &["card_game_always_win"],
            Self::Stats => &[],
        }
    }
    #[cfg(test)]
    pub fn variants(self) -> Vec<Variant> {
        super::config::Execution::default().groups[&format!("{self:?}")].clone()
    }
    #[cfg(test)]
    pub fn layout_pattern(self) -> Option<String> {
        super::config::Execution::default()
            .layouts
            .get(&format!("{self:?}"))
            .cloned()
    }
}

/// Verify entire instructions before relocating their bytes. These recipes never
/// contain RIP-relative operands or branches in the overwritten span.
pub fn validate_original(group: Group, bytes: &[u8]) -> Result<(), String> {
    if bytes.len() < 5 || bytes.len() > 16 {
        return Err("目标指令长度无效".into());
    }
    if group == Group::Battle
        && (bytes.len() != 7 || i32::from_le_bytes(bytes[2..6].try_into().unwrap()) < 4)
    {
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
#[cfg(test)]
pub fn build(
    group: Group,
    original: &[u8],
    cave: usize,
    return_address: usize,
) -> Result<Vec<u8>, String> {
    build_configured(
        group,
        original,
        cave,
        return_address,
        &super::config::Execution::default(),
    )
}

pub fn build_configured(
    group: Group,
    original: &[u8],
    cave: usize,
    return_address: usize,
    config: &super::config::Execution,
) -> Result<Vec<u8>, String> {
    validate_original(group, original)?;
    assemble(group, original, cave as u64, return_address as u64, config).map_err(|e| e.to_string())
}

fn scaled_integer(
    a: &mut CodeAssembler,
    field: AsmMemoryOperand,
    parameter: AsmMemoryOperand,
    divide: bool,
) -> Result<(), iced_x86::IcedError> {
    let mut skip = a.create_label();
    let mut upper_ok = a.create_label();
    let mut lower_ok = a.create_label();
    a.cmp(parameter, 0)?;
    a.jle(skip)?;
    a.push(rax)?;
    a.fild(field)?;
    if divide {
        a.fdiv(parameter)?;
    } else {
        a.fmul(parameter)?;
    }
    a.sub(rsp, 8)?;
    a.fistp(qword_ptr(rsp))?;
    a.mov(rax, qword_ptr(rsp))?;
    a.add(rsp, 8)?;
    // Avoid the original's i32 overflow for very large reward/damage multipliers.
    a.cmp(rax, i32::MAX)?;
    a.jle(upper_ok)?;
    a.mov(rax, i32::MAX as i64)?;
    a.set_label(&mut upper_ok)?;
    a.cmp(rax, i32::MIN)?;
    a.jge(lower_ok)?;
    a.mov(rax, i32::MIN as i64)?;
    a.set_label(&mut lower_ok)?;
    a.mov(field, eax)?;
    a.pop(rax)?;
    a.set_label(&mut skip)?;
    Ok(())
}

fn assemble(
    group: Group,
    original: &[u8],
    cave: u64,
    return_address: u64,
    config: &super::config::Execution,
) -> Result<Vec<u8>, iced_x86::IcedError> {
    let f = &config.fields;
    let limits = &config.limits;
    let mut a = CodeAssembler::new(64)?;
    a.pushfq()?;
    a.push(r11)?;
    a.mov(r11, cave + 0x1000)?;
    let mut done = a.create_label();
    match group {
        Group::Battle => {
            let displacement = i32::from_le_bytes(original[2..6].try_into().unwrap());
            a.cmp(dword_ptr(rdi + displacement + f["battle_team_delta"]), 1)?;
            a.jne(done)?;
            let mut sp_label = a.create_label();
            a.cmp(dword_ptr(r11), 1)?;
            a.jne(sp_label)?;
            a.mov(
                dword_ptr(rdi + displacement + f["battle_hp_delta"]),
                limits["battle"],
            )?;
            a.set_label(&mut sp_label)?;
            a.cmp(dword_ptr(r11 + 4), 1)?;
            a.jne(done)?;
            a.mov(
                dword_ptr(rdi + displacement + f["battle_sp_delta"]),
                limits["battle"],
            )?;
        }
        Group::Cp => {
            a.push(rax)?;
            if original[0] == 0x41 {
                a.mov(eax, dword_ptr(r13 + f["cp_max"]))?;
                a.mov(dword_ptr(r13 + f["cp_current"]), eax)?;
            } else {
                a.mov(eax, dword_ptr(rcx + f["cp_max"]))?;
                a.mov(dword_ptr(rcx + f["cp_current"]), eax)?;
            }
            a.pop(rax)?;
        }
        Group::Scan => {
            a.cmp(dword_ptr(rbx + f["scan_value"]), 0)?;
            a.jle(done)?;
            a.mov(dword_ptr(rbx + f["scan_value"]), limits["scan"])?;
        }
        Group::Damage => {
            let mut enemy = a.create_label();
            a.test(rcx, rcx)?;
            a.je(done)?;
            a.cmp(dword_ptr(rcx + f["damage_team"]), 2)?;
            a.je(enemy)?;
            scaled_integer(
                &mut a,
                dword_ptr(rdx + f["damage_value"]),
                dword_ptr(r11 + 8),
                true,
            )?;
            a.jmp(done)?;
            a.set_label(&mut enemy)?;
            scaled_integer(
                &mut a,
                dword_ptr(rdx + f["damage_value"]),
                dword_ptr(r11 + 4),
                false,
            )?;
            a.cmp(dword_ptr(r11), 1)?;
            a.jne(done)?;
            a.mov(dword_ptr(rdx + f["damage_value"]), limits["damage"])?;
        }
        Group::Items => {
            let mut other = a.create_label();
            let mut set_amount = a.create_label();
            let mut end_items = a.create_label();
            a.push(rbx)?;
            a.mov(ebx, dword_ptr(r14 + f["item_kind"]))?;
            a.cmp(ebx, 3)?;
            a.jne(other)?;
            a.mov(edx, dword_ptr(r15 + f["item_category"]))?;
            for index in 0..6 {
                let mut next = a.create_label();
                a.cmp(edx, index)?;
                a.jne(next)?;
                a.mov(edx, dword_ptr(r11 + index * 4))?;
                a.jmp(set_amount)?;
                a.set_label(&mut next)?;
            }
            a.jmp(end_items)?;
            a.set_label(&mut other)?;
            let mut equipment = a.create_label();
            a.cmp(ebx, 1)?;
            a.jne(equipment)?;
            a.mov(edx, dword_ptr(r11 + 24))?;
            a.jmp(set_amount)?;
            a.set_label(&mut equipment)?;
            a.cmp(ebx, 2)?;
            a.jne(end_items)?;
            a.mov(edx, dword_ptr(r11 + 12))?;
            a.set_label(&mut set_amount)?;
            a.test(edx, edx)?;
            a.jle(end_items)?;
            a.mov(dword_ptr(rax + f["item_count"]), edx)?;
            a.set_label(&mut end_items)?;
            a.pop(rbx)?;
        }
        Group::Rewards => {
            scaled_integer(
                &mut a,
                dword_ptr(r15 + f["reward_money"]),
                dword_ptr(r11),
                false,
            )?;
            scaled_integer(
                &mut a,
                dword_ptr(r15 + f["reward_exp"]),
                dword_ptr(r11 + 8),
                false,
            )?;
            a.cmp(dword_ptr(r11 + 4), 1)?;
            a.jne(done)?;
            a.mov(dword_ptr(r15 + f["reward_exp"]), limits["exp"])?;
        }
        Group::Evolution => {
            for offset in config.evolution_offsets.iter().copied() {
                a.mov(dword_ptr(rdx + offset), 0)?;
            }
        }
        Group::Bond => {
            a.comiss(xmm6, dword_ptr(r11 + 0x100))?;
            a.jbe(done)?;
            let mut maximum = a.create_label();
            a.cmp(dword_ptr(r11 + 4), 0)?;
            a.jle(maximum)?;
            a.mulss(xmm6, dword_ptr(r11 + 4))?;
            a.set_label(&mut maximum)?;
            a.cmp(dword_ptr(r11), 1)?;
            a.jne(done)?;
            a.movss(xmm6, dword_ptr(r11 + 0x104))?;
        }
        Group::CardRewards => {
            a.test(eax, eax)?;
            a.jle(done)?;
            a.mov(eax, limits["card_rewards"])?;
            a.pop(r11)?;
            a.popfq()?;
            a.cmp(eax, 0)?;
            a.jmp(return_address)?;
        }
        Group::CardWin => {
            a.xor(eax, eax)?;
        }
        Group::Stats => {
            a.mov(qword_ptr(r11), rdi)?;
            a.mov(edx, dword_ptr(rdi + f["object_id"]))?;
            a.mov(dword_ptr(r11 + 8), edx)?;
        }
    }
    a.set_label(&mut done)?;
    a.pop(r11)?;
    a.popfq()?;
    a.db(original)?;
    a.jmp(return_address)?;
    a.assemble(cave)
}

/// One-shot fields from the sample's native editor callback (RVA 0x53960).
/// Talent uses two fixed-point fields; bond uses a float scaled by 100.
#[cfg(test)]
pub fn stat_writes(id: &str, value: i32) -> Result<Vec<(usize, [u8; 4])>, String> {
    super::config::Execution::default().stat_writes(id, value)
}
