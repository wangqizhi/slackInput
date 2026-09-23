use super::{
    Target,
    native::Session,
    profile::{self, ImportDraft, Profile, Repository, StaticImporter},
};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

enum Command {
    Bind {
        target: Option<Target>,
        epoch: u64,
    },
    Set {
        epoch: u64,
        id: String,
        value: Option<f64>,
        enabled: bool,
    },
    StopAll {
        exit: bool,
    },
    Analyze {
        epoch: u64,
    },
    DismissDraft,
    Save {
        epoch: u64,
        profile: Box<Profile>,
    },
    Shutdown,
}

#[derive(Default, Clone)]
pub struct Snapshot {
    pub draft: Option<ImportDraft>,
    pub epoch: u64,
    pub target: Option<Target>,
    pub profile: Option<Profile>,
    pub active: BTreeMap<String, Option<f64>>,
    pub applied: BTreeMap<String, (f64, u32)>,
    pub message: String,
    pub error: bool,
    pub busy: bool,
    pub exit_ready: bool,
    pub cleanup_failed: bool,
}

pub struct Worker {
    sender: mpsc::Sender<Command>,
    receiver: mpsc::Receiver<Snapshot>,
    cancel: Arc<AtomicU64>,
    thread: Option<thread::JoinHandle<()>>,
    pub state: Snapshot,
}
impl Worker {
    pub fn new(directory: PathBuf) -> Self {
        let (sender, commands) = mpsc::channel();
        let (updates, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicU64::new(0));
        let token = cancel.clone();
        let thread = thread::spawn(move || run(commands, updates, token, Repository { directory }));
        let mut worker = Self {
            sender,
            receiver,
            cancel,
            thread: Some(thread),
            state: Snapshot::default(),
        };
        worker.bind(None);
        worker
    }
    pub fn bind(&mut self, target: Option<Target>) {
        let epoch = self.cancel.fetch_add(1, Ordering::AcqRel) + 1;
        self.state.busy = true;
        let _ = self.sender.send(Command::Bind { target, epoch });
    }
    pub fn set(&mut self, id: String, value: Option<f64>, enabled: bool) {
        self.state.busy = true;
        let _ = self.sender.send(Command::Set {
            epoch: self.state.epoch,
            id,
            value,
            enabled,
        });
    }
    pub fn stop_all(&mut self, exit: bool) {
        // Cleanup is serialized after an in-flight operation.
        self.state.busy = true;
        self.state.exit_ready = false;
        let _ = self.sender.send(Command::StopAll { exit });
    }
    pub fn dismiss_draft(&mut self) {
        self.state.draft = None;
        self.state.busy = true;
        let _ = self.sender.send(Command::DismissDraft);
    }
    pub fn analyze(&mut self) {
        self.state.busy = true;
        let _ = self.sender.send(Command::Analyze {
            epoch: self.state.epoch,
        });
    }
    pub fn save(&mut self, profile: Profile) {
        self.state.busy = true;
        let _ = self.sender.send(Command::Save {
            epoch: self.state.epoch,
            profile: Box::new(profile),
        });
    }
    pub fn poll(&mut self) {
        while let Ok(state) = self.receiver.try_recv() {
            self.state = state;
        }
        if self.thread.as_ref().is_some_and(|t| t.is_finished()) && !self.state.exit_ready {
            self.state.busy = false;
            self.state.error = true;
            self.state.cleanup_failed = true;
            self.state.message = "修改器工作线程已停止，请退出游戏后重启程序".into();
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel.fetch_add(1, Ordering::AcqRel);
        let _ = self.sender.send(Command::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(
    commands: mpsc::Receiver<Command>,
    updates: mpsc::Sender<Snapshot>,
    cancel: Arc<AtomicU64>,
    repository: Repository,
) {
    let mut state = Snapshot::default();
    let mut session: Option<Session> = None;
    loop {
        // Stop existing effects even when the trainer window is closed.
        if crate::app_state().lock().unwrap().features_locked() && session.is_some() {
            let result = session.as_mut().unwrap().cleanup();
            if let Err(error) = result {
                state.cleanup_failed = true;
                state.error = true;
                state.message = format!("反作弊限制：恢复修改失败：{error}");
            } else {
                session = None;
                state.active.clear();
                state.applied.clear();
                state.cleanup_failed = false;
                state.message = "检测到反作弊进程，修改器已停用".into();
            }
            let _ = updates.send(state.clone());
        }
        let command = match commands.recv_timeout(Duration::from_millis(200)) {
            Ok(command) => command,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if session.as_ref().is_some_and(Session::exited) {
                    session = None;
                    state.active.clear();
                    state.applied.clear();
                    state.message = "游戏已退出，所有修改已停用".into();
                    state.cleanup_failed = false;
                    state.error = false;
                    let _ = updates.send(state.clone());
                }
                continue;
            }
            Err(_) => break,
        };
        let shutdown = matches!(command, Command::Shutdown);
        let result: Result<(), String> = (|| {
            match command {
                Command::Bind { target, epoch } => {
                    if let Some(old) = &mut session
                        && let Err(e) = old.cleanup()
                    {
                        state.cleanup_failed = true;
                        return Err(format!("旧进程的修改未能全部恢复，请重试停用全部：{e}"));
                    }
                    session = None;
                    state = Snapshot {
                        target: target.clone(),
                        epoch,
                        ..Default::default()
                    };
                    if let Some(target) = target {
                        state.profile = repository.resolve(&target.name)?;
                        state.message = if state.profile.is_some() {
                            "已加载修改器配置，默认关闭；实际效果请在游戏中确认"
                        } else {
                            "当前进程没有匹配的修改器配置"
                        }
                        .into();
                    } else {
                        state.message = "请先选择游戏进程".into();
                    }
                }
                Command::DismissDraft => {
                    state.draft = None;
                }
                Command::Analyze { epoch } => {
                    if epoch != cancel.load(Ordering::Acquire) {
                        return Err("绑定已变化".into());
                    }
                    state.draft = None;
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("修改器 EXE", &["exe"])
                        .pick_file()
                    {
                        use std::io::Read;
                        let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
                        let mut bytes = Vec::new();
                        file.take(32 * 1024 * 1024 + 1)
                            .read_to_end(&mut bytes)
                            .map_err(|e| e.to_string())?;
                        let draft = profile::SampleImporter.analyze(&bytes)?;
                        draft.profile.validate()?;
                        if let Some(target) = &state.target
                            && !draft.profile.matches_process(&target.name)
                        {
                            return Err("导入配置与已绑定游戏不匹配".into());
                        }
                        state.draft = Some(draft);
                        state.message = "请选择同名项覆盖或跳过，然后确认导入".into();
                    }
                }
                Command::Save { epoch, profile } => {
                    if epoch != state.epoch || epoch != cancel.load(Ordering::Acquire) {
                        return Err("绑定已变化，请重新操作".into());
                    }
                    profile.validate()?;
                    if let Some(target) = &state.target
                        && !profile.matches_process(&target.name)
                    {
                        return Err("配置与绑定进程不匹配".into());
                    }
                    if let Some(old) = &mut session
                        && let Err(e) = old.cleanup()
                    {
                        state.cleanup_failed = true;
                        return Err(e);
                    }
                    session = None;
                    state.cleanup_failed = false;
                    repository.save(&profile)?;
                    state.profile = Some(*profile);
                    state.draft = None;
                    state.message = "配置已保存，所有修改项保持关闭".into();
                }
                Command::Set {
                    epoch,
                    id,
                    value,
                    enabled,
                } => {
                    state.exit_ready = false;
                    if epoch != state.epoch || epoch != cancel.load(Ordering::Acquire) {
                        return Err("绑定已变化，旧操作已取消".into());
                    }
                    if state.cleanup_failed {
                        return Err("请先重试停用全部，恢复旧进程的修改".into());
                    }
                    let profile = state.profile.as_ref().ok_or("未加载修改器")?;
                    let feature = profile
                        .features
                        .iter()
                        .find(|f| f.id == id)
                        .ok_or("未知修改项")?;
                    if !profile::supported(profile, &id) {
                        return Err("该功能待适配，尚不能启用".into());
                    }
                    if enabled {
                        if crate::app_state().lock().unwrap().features_locked() {
                            return Err("检测到反作弊进程，修改器已锁定".into());
                        }
                        if let Some(v) = value {
                            feature.parse_value(&v.to_string())?;
                        }
                        if !cfg!(test) && std::env::var_os("SLACKINPUT_UI_SNAPSHOT").is_some() {
                            return Err("截图模式不执行修改".into());
                        }
                        match crate::anti_cheat::scan().outcome {
                            crate::anti_cheat::Outcome::NoKnownProcess => {}
                            crate::anti_cheat::Outcome::Detected(_) => {
                                return Err("检测到已知反作弊进程，未执行修改".into());
                            }
                            crate::anti_cheat::Outcome::Unavailable(e) => {
                                return Err(format!("无法检查进程环境：{e}"));
                            }
                        }
                        if session.is_none() {
                            session = Some(Session::open(
                                state.target.as_ref().ok_or("未绑定游戏")?,
                                cancel.clone(),
                                epoch,
                            )?);
                        }
                    }
                    if let Some(session) = &mut session {
                        session.config = profile.execution.clone();
                        if let Err(e) = session.set(&id, value, enabled) {
                            // A failed write can leave a journal entry even if no UI
                            // toggle was confirmed. Keep the session for cleanup.
                            state.cleanup_failed = session.needs_cleanup();
                            return Err(e);
                        }
                    }
                    state.message = if feature.one_shot() && enabled {
                        "已应用到读取到的数码宝贝；离开并重新打开属性界面以刷新显示"
                    } else if enabled {
                        "已启用；请在游戏中的相应场景确认效果"
                    } else {
                        "已停用；已经修改的资源和属性不会自动回退"
                    }
                    .into();
                }
                Command::StopAll { exit } => {
                    if let Some(s) = &mut session
                        && let Err(e) = s.cleanup()
                    {
                        state.cleanup_failed = true;
                        return Err(e);
                    }
                    state.cleanup_failed = false;
                    state.exit_ready = exit;
                    state.message = "已停用全部修改".into();
                }
                Command::Shutdown => {
                    if let Some(s) = &mut session {
                        s.cleanup()?;
                    }
                }
            }
            Ok(())
        })();
        state.busy = false;
        state.active = session
            .as_ref()
            .map(|s| s.active.clone())
            .unwrap_or_default();
        state.applied = session
            .as_ref()
            .map(|s| s.applied.clone())
            .unwrap_or_default();
        state.error = result.is_err();
        if let Err(e) = result {
            state.message = e;
        }
        let _ = updates.send(state.clone());
        if shutdown {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn serialized_save_rejects_stale_epoch_and_reloads_empty_catalog() {
        let directory = std::env::temp_dir().join(format!("trainer-worker-{}", std::process::id()));
        let repo = Repository {
            directory: directory.clone(),
        };
        let (send, commands) = mpsc::channel();
        let (updates, recv) = mpsc::channel();
        let cancel = Arc::new(AtomicU64::new(1));
        let token = cancel.clone();
        let thread = thread::spawn(move || run(commands, updates, token, repo));
        let target = Target {
            pid: std::process::id(),
            name: profile::GAME_PROCESS.into(),
            generation: 1,
            created: 0,
        };
        send.send(Command::Bind {
            target: Some(target.clone()),
            epoch: 1,
        })
        .unwrap();
        let initial = recv.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(initial.profile.as_ref().unwrap().features.len(), 40);
        let mut empty = initial.profile.unwrap();
        empty.features.clear();
        send.send(Command::Save {
            epoch: 0,
            profile: Box::new(empty.clone()),
        })
        .unwrap();
        assert!(recv.recv_timeout(Duration::from_secs(5)).unwrap().error);
        send.send(Command::Save {
            epoch: 1,
            profile: Box::new(empty),
        })
        .unwrap();
        let saved = recv.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(!saved.error);
        assert!(saved.active.is_empty());
        assert!(saved.profile.unwrap().features.is_empty());
        cancel.store(2, Ordering::Release);
        send.send(Command::Bind {
            target: None,
            epoch: 2,
        })
        .unwrap();
        let unbound = recv.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(unbound.profile.is_none());
        cancel.store(3, Ordering::Release);
        send.send(Command::Bind {
            target: Some(target),
            epoch: 3,
        })
        .unwrap();
        assert!(
            recv.recv_timeout(Duration::from_secs(5))
                .unwrap()
                .profile
                .unwrap()
                .features
                .is_empty()
        );
        send.send(Command::Shutdown).unwrap();
        thread.join().unwrap();
        std::fs::remove_file(directory.join(format!("{}.json", profile::builtin().id))).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
}
