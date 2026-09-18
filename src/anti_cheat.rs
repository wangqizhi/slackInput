//! Read-only, system-wide process-name hints; never a guarantee that editing is safe.
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, ERROR_NO_MORE_FILES, HANDLE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};

pub const REFRESH_INTERVAL: Duration = Duration::from_secs(2);
pub const MAX_AGE: Duration = Duration::from_secs(6);
// Initial coverage only. Names are hints, not authenticated vendor identities.
// Sources and coverage limitations: docs/game-data-editing-plan.md.
const RULES: &[(&str, &str)] = &[
    ("beservice.exe", "BattlEye"),
    ("beservice_x64.exe", "BattlEye"),
    ("easyanticheat.exe", "Easy Anti-Cheat"),
    ("easyanticheat_eos.exe", "Easy Anti-Cheat EOS"),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    NoKnownProcess,
    Detected(Vec<String>),
    Unavailable(String),
}

#[derive(Clone)]
pub struct Report {
    pub outcome: Outcome,
    pub checked_at: Instant,
}

impl Report {
    pub fn fresh(&self) -> bool {
        self.checked_at.elapsed() <= MAX_AGE
    }
}

struct Snapshot(HANDLE);
impl Drop for Snapshot {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn process_names() -> windows::core::Result<Vec<String>> {
    let snapshot = Snapshot(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)? });
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    unsafe { Process32FirstW(snapshot.0, &mut entry)? };
    let mut names = Vec::new();
    loop {
        let end = entry
            .szExeFile
            .iter()
            .position(|&ch| ch == 0)
            .unwrap_or(entry.szExeFile.len());
        names.push(String::from_utf16_lossy(&entry.szExeFile[..end]));
        if let Err(error) = unsafe { Process32NextW(snapshot.0, &mut entry) } {
            if error.code() != ERROR_NO_MORE_FILES.to_hresult() {
                return Err(error);
            }
            break;
        }
    }
    Ok(names)
}

fn classify(names: Result<Vec<String>, String>) -> Outcome {
    let names = match names {
        Ok(names) if !names.is_empty() => names,
        Ok(_) => return Outcome::Unavailable("Empty process snapshot".into()),
        Err(error) => return Outcome::Unavailable(error),
    };
    let mut matches = Vec::new();
    for name in names {
        if let Some((executable, vendor)) =
            RULES.iter().find(|(exe, _)| name.eq_ignore_ascii_case(exe))
        {
            matches.push(format!("{vendor}: {executable}"));
        }
    }
    matches.sort();
    matches.dedup();
    if matches.is_empty() {
        Outcome::NoKnownProcess
    } else {
        Outcome::Detected(matches)
    }
}

pub fn scan() -> Report {
    Report {
        outcome: classify(process_names().map_err(|error| error.to_string())),
        checked_at: Instant::now(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn known_names_match_case_insensitively_and_deduplicate() {
        for (name, _) in RULES {
            assert!(
                matches!(classify(Ok(vec![name.to_uppercase(), name.to_string()])),
                Outcome::Detected(items) if items.len() == 1)
            );
        }
    }
    #[test]
    fn failures_and_empty_snapshots_are_never_clear() {
        assert!(matches!(
            classify(Err("denied".into())),
            Outcome::Unavailable(_)
        ));
        assert!(matches!(classify(Ok(vec![])), Outcome::Unavailable(_)));
    }
    #[test]
    fn unrelated_and_similar_names_do_not_match() {
        assert_eq!(
            classify(Ok(vec![
                "game.exe".into(),
                "not-beservice.exe".into(),
                "EasyAntiCheat_EOS_Setup.exe".into()
            ])),
            Outcome::NoKnownProcess
        );
    }
    #[test]
    fn stale_reports_are_not_fresh() {
        let report = Report {
            outcome: Outcome::NoKnownProcess,
            checked_at: Instant::now() - MAX_AGE - Duration::from_secs(1),
        };
        assert!(!report.fresh());
    }
    #[test]
    fn native_snapshot_includes_own_process_without_opening_process_handles() {
        let own_name = std::env::current_exe()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            process_names()
                .unwrap()
                .iter()
                .any(|name| name.eq_ignore_ascii_case(&own_name))
        );
    }
}
