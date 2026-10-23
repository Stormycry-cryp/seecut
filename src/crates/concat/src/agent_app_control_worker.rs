//! 单线程 journal 队列。宿主传可信根与 scope；这里不接导航或许可回调。
use crate::agent_app_control_core::{Error, Intent, Record, RunningProof, RunningWrite};
use crate::agent_app_control_journal::{
    ExecutionFence, Journal, Prepared, QueryScope, RecoveryView, Saved,
};
use std::{
    io,
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, Sender, SyncSender, TryRecvError, TrySendError},
    },
    thread,
};

/// 最多排队 32 项，另有一个正在处理的请求。投递从不等待磁盘或队列空位。
pub const QUEUE_CAPACITY: usize = 32;
pub type Reply<T> = Receiver<Result<T, WorkerError>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerError {
    Core(Error),
    WorkerStopped,
}
impl From<Error> for WorkerError {
    fn from(error: Error) -> Self {
        Self::Core(error)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubmitFailure {
    QueueFull,
    WorkerStopped,
}
/// 未投递的原输入原样退回；不表示保存成功，也不产生重放或执行许可。
pub struct SubmitError<T> {
    pub kind: SubmitFailure,
    pub input: T,
}

/// UI 定时 drain：None 表示尚未完成，断线明确返回 WorkerStopped。
pub fn poll_reply<T>(reply: &Reply<T>) -> Option<Result<T, WorkerError>> {
    match reply.try_recv() {
        Ok(result) => Some(result),
        Err(TryRecvError::Empty) => None,
        Err(TryRecvError::Disconnected) => Some(Err(WorkerError::WorkerStopped)),
    }
}

enum Command {
    Prepare(Intent, Sender<Result<Prepared, WorkerError>>),
    PersistRunning(RunningWrite, Sender<Result<RunningProof, WorkerError>>),
    CancelPrepared(Prepared, Sender<Result<RecoveryView, WorkerError>>),
    Save(Record, Sender<Result<Saved, WorkerError>>),
    ConfirmQuery(String, Sender<Result<RecoveryView, WorkerError>>),
    Close,
    #[cfg(test)]
    Crash,
    #[cfg(test)]
    Pause(Sender<()>, Receiver<()>),
}

#[derive(Default)]
struct FenceState {
    fenced: bool,
    journal: Option<ExecutionFence>,
}
#[derive(Default)]
struct FenceBridge(Mutex<FenceState>);
impl FenceBridge {
    fn fence(&self) {
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        state.fenced = true;
        if let Some(fence) = &state.journal {
            fence.fence();
        }
    }
    fn install(&self, fence: ExecutionFence) {
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if state.fenced {
            fence.fence();
        }
        state.journal = Some(fence);
    }
    fn execution_allowed(&self) -> Result<(), WorkerError> {
        let state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if state.fenced {
            Err(Error::Busy.into())
        } else {
            Ok(())
        }
    }
}

/// 唯一投递者，不可 Clone。Drop 只撤执行和通知关闭，不 join 后台线程。
/// Arc 里仅保存 weak execution fence；Journal 始终只归 worker 局部变量所有。
pub struct Handle {
    sender: Option<SyncSender<Command>>,
    fence: Arc<FenceBridge>,
    closed: Mutex<Option<Reply<()>>>,
}
impl Handle {
    /// 在 open 返回前也立即记住撤销；安装迟到的真实 fence 时同步补上。
    pub fn fence(&self) {
        self.fence.fence();
    }
    pub fn prepare(&self, intent: Intent) -> Result<Reply<Prepared>, Box<SubmitError<Intent>>> {
        self.submit(intent, Command::Prepare, |command| match command {
            Command::Prepare(input, _) => input,
            _ => unreachable!(),
        })
    }
    /// Consume a fresh unadmitted Prepared; only the worker can confirm its cancellation.
    /// Submission failure returns the same capability for explicit caller handling.
    pub fn cancel_prepared(
        &self,
        prepared: Prepared,
    ) -> Result<Reply<RecoveryView>, Box<SubmitError<Prepared>>> {
        self.submit(prepared, Command::CancelPrepared, |command| match command {
            Command::CancelPrepared(input, _) => input,
            _ => unreachable!(),
        })
    }
    pub fn persist_running(
        &self,
        write: RunningWrite,
    ) -> Result<Reply<RunningProof>, Box<SubmitError<RunningWrite>>> {
        self.submit(write, Command::PersistRunning, |command| match command {
            Command::PersistRunning(input, _) => input,
            _ => unreachable!(),
        })
    }
    pub fn save(&self, record: Record) -> Result<Reply<Saved>, Box<SubmitError<Record>>> {
        self.submit(record, Command::Save, |command| match command {
            Command::Save(input, _) => input,
            _ => unreachable!(),
        })
    }
    /// 只确认原 operationId；实际 Saved::into_recovery 在持锁线程内调用。
    pub fn confirm_query(
        &self,
        original_id: String,
    ) -> Result<Reply<RecoveryView>, Box<SubmitError<String>>> {
        self.submit(
            original_id,
            Command::ConfirmQuery,
            |command| match command {
                Command::ConfirmQuery(input, _) => input,
                _ => unreachable!(),
            },
        )
    }
    fn submit<T, R>(
        &self,
        input: T,
        build: impl FnOnce(T, Sender<Result<R, WorkerError>>) -> Command,
        recover: impl FnOnce(Command) -> T,
    ) -> Result<Reply<R>, Box<SubmitError<T>>> {
        let Some(sender) = &self.sender else {
            return Err(Box::new(SubmitError {
                kind: SubmitFailure::WorkerStopped,
                input,
            }));
        };
        let (answer, reply) = mpsc::channel();
        match sender.try_send(build(input, answer)) {
            Ok(()) => Ok(reply),
            Err(TrySendError::Full(command)) => Err(Box::new(SubmitError {
                kind: SubmitFailure::QueueFull,
                input: recover(command),
            })),
            Err(TrySendError::Disconnected(command)) => Err(Box::new(SubmitError {
                kind: SubmitFailure::WorkerStopped,
                input: recover(command),
            })),
        }
    }
    /// 立即返回独立完成通道；仅在 Journal Drop 后回报成功。
    /// 关闭前已入队的请求按 FIFO 处理。调用方先投递全部收尾事实再 close。
    pub fn close(mut self) -> Reply<()> {
        self.stop();
        self.closed
            .get_mut()
            .unwrap_or_else(|error| error.into_inner())
            .take()
            .expect("唯一关闭回执")
    }
    fn stop(&mut self) {
        self.fence();
        if let Some(sender) = self.sender.take() {
            // 队列满时 drop 唯一 sender；worker 排空后收到断线，同样真实关闭。
            let _ = sender.try_send(Command::Close);
        }
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// 这里只创建后台线程，不在调用线程打开目录或 journal。
/// Ready 通道返回真实 open 结果；线程创建失败保留标准 io::Error。
/// 可信根与 QueryScope 由宿主构造，禁止直接转发模型路径或身份参数。
pub fn spawn(root: PathBuf, scope: QueryScope) -> io::Result<(Handle, Reply<()>)> {
    spawn_inner(root, scope, Hooks::default())
}

#[cfg(test)]
pub(crate) fn spawn_gated(
    root: PathBuf,
    scope: QueryScope,
    before_open: Option<Receiver<()>>,
    after_open: Option<Receiver<()>>,
) -> io::Result<(Handle, Reply<()>)> {
    spawn_inner(
        root,
        scope,
        Hooks {
            before_open,
            after_open,
        },
    )
}

#[cfg(test)]
impl Handle {
    pub(crate) fn crash_for_test(&self) {
        assert!(
            self.sender
                .as_ref()
                .unwrap()
                .try_send(Command::Crash)
                .is_ok()
        );
    }
    pub(crate) fn pause_for_test(&self, entered: Sender<()>, release: Receiver<()>) {
        assert!(
            self.sender
                .as_ref()
                .unwrap()
                .try_send(Command::Pause(entered, release))
                .is_ok()
        );
    }
    pub(crate) fn take_closed_for_drop_test(&mut self) -> Reply<()> {
        self.closed.get_mut().unwrap().take().unwrap()
    }
}

#[derive(Default)]
struct Hooks {
    #[cfg(test)]
    before_open: Option<Receiver<()>>,
    #[cfg(test)]
    after_open: Option<Receiver<()>>,
}
fn spawn_inner(root: PathBuf, scope: QueryScope, hooks: Hooks) -> io::Result<(Handle, Reply<()>)> {
    let (sender, commands) = mpsc::sync_channel(QUEUE_CAPACITY);
    let (opened, ready) = mpsc::channel();
    let (finished, closed) = mpsc::channel();
    let fence = Arc::new(FenceBridge::default());
    let worker_fence = Arc::clone(&fence);
    thread::Builder::new()
        .name("app-control-journal".into())
        .spawn(move || {
            // unwind 会先释放局部 Journal；普通关闭也在 run 返回前释放。
            let result = catch_unwind(AssertUnwindSafe(|| {
                run(root, scope, commands, opened, worker_fence, hooks)
            }))
            .unwrap_or(Err(WorkerError::WorkerStopped));
            let _ = finished.send(result);
        })?;
    Ok((
        Handle {
            sender: Some(sender),
            fence,
            closed: Mutex::new(Some(closed)),
        },
        ready,
    ))
}
fn run(
    root: PathBuf,
    scope: QueryScope,
    commands: Receiver<Command>,
    opened: Sender<Result<(), WorkerError>>,
    fence: Arc<FenceBridge>,
    _hooks: Hooks,
) -> Result<(), WorkerError> {
    #[cfg(test)]
    if let Some(gate) = _hooks.before_open {
        gate.recv().expect("测试 open gate");
    }
    let mut journal = match Journal::open(&root, scope) {
        Ok(journal) => journal,
        Err(error) => {
            let error = WorkerError::Core(error);
            let _ = opened.send(Err(error));
            return Err(error);
        }
    };
    fence.install(journal.execution_fence());
    let _ = opened.send(Ok(()));
    #[cfg(test)]
    if let Some(gate) = _hooks.after_open {
        gate.recv().expect("测试 command gate");
    }
    while let Ok(command) = commands.recv() {
        match command {
            Command::Prepare(intent, answer) => {
                let result = fence
                    .execution_allowed()
                    .and_then(|()| journal.prepare(intent).map_err(Into::into));
                let _ = answer.send(result);
            }
            Command::CancelPrepared(prepared, answer) => {
                let result = journal
                    .cancel_prepared(prepared)
                    .and_then(Saved::into_recovery);
                let _ = answer.send(result.map_err(Into::into));
            }
            Command::PersistRunning(write, answer) => {
                // 已投递的 rev2 必须先于后续取消事实落盘；fence 后的 proof
                // 仍由核心 owner 拒绝 commit，不能获得执行能力。
                let _ = answer.send(journal.persist_running(write).map_err(Into::into));
            }
            Command::Save(record, answer) => {
                let _ = answer.send(journal.save(record).map_err(Into::into));
            }
            Command::ConfirmQuery(id, answer) => {
                let result = journal.confirm_query(&id).and_then(Saved::into_recovery);
                let _ = answer.send(result.map_err(Into::into));
            }
            Command::Close => break,
            #[cfg(test)]
            Command::Crash => panic!("测试 worker 异常退出"),
            #[cfg(test)]
            Command::Pause(entered, release) => {
                entered.send(()).unwrap();
                release.recv().unwrap();
            }
        }
    }
    drop(journal);
    Ok(())
}

#[cfg(test)]
#[path = "agent_app_control_worker_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "agent_app_control_unadmitted_cancel_tests.rs"]
mod unadmitted_cancel_tests;
