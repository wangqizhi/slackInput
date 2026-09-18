use super::{
    Target,
    native::Session,
    profile::{self, Profile, Repository},
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
    Shutdown,
}

#[derive(Default, Clone)]
pub struct Snapshot {
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
        Self {
            sender,
            receiver,
            cancel,
            thread: Some(thread),
            state: Snapshot::default(),
        }
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
                    if let Some(old) = &mut session {
                        if let Err(e) = old.cleanup() {
                            state.cleanup_failed = true;
                            return Err(format!("旧进程的修改未能全部恢复，请重试停用全部：{e}"));
                        }
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
                            "已加载 40 项修改适配，默认关闭；实际效果请在游戏中确认"
                        } else {
                            "当前进程没有匹配的修改器配置"
                        }
                        .into();
                    } else {
                        state.message = "请先绑定游戏进程".into();
                    }
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
                    if let Some(s) = &mut session {
                        if let Err(e) = s.cleanup() {
                            state.cleanup_failed = true;
                            return Err(e);
                        }
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
