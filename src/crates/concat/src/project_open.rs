// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bounded project I/O and logical request lifetime. Cancellation never frees a
//! permit held by a thread still inside a filesystem call.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

pub(crate) struct Capacity {
    active: AtomicUsize,
    limit: usize,
}
impl Capacity {
    pub(crate) const fn new(limit: usize) -> Self {
        Self {
            active: AtomicUsize::new(0),
            limit,
        }
    }
    pub(crate) fn acquire(&self) -> Option<Permit<'_>> {
        self.active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                if active < self.limit {
                    Some(active + 1)
                } else {
                    None
                }
            })
            .ok()
            .map(|_| Permit(self))
    }
}
pub(crate) struct Permit<'a>(&'a Capacity);
impl Drop for Permit<'_> {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
    }
}
static OPEN_IO: Capacity = Capacity::new(2);
static CACHE_IO: Capacity = Capacity::new(2);
static RECENT_IO: Capacity = Capacity::new(1);

#[derive(Debug)]
pub(crate) enum DispatchError {
    Busy,
    Spawn(std::io::Error),
}

pub(crate) fn dispatch(
    cache: bool,
    work: impl FnOnce() + Send + 'static,
) -> Result<(), DispatchError> {
    let capacity = if cache { &CACHE_IO } else { &OPEN_IO };
    dispatch_from(capacity, work).map(|_| ())
}

/// Serialize recent-list read/modify/write without blocking the loader slots.
pub(crate) fn dispatch_recent(work: impl FnOnce() + Send + 'static) -> Result<(), DispatchError> {
    dispatch_from(&RECENT_IO, work).map(|_| ())
}

fn dispatch_from(
    capacity: &'static Capacity,
    work: impl FnOnce() + Send + 'static,
) -> Result<std::thread::JoinHandle<()>, DispatchError> {
    let permit = capacity.acquire().ok_or(DispatchError::Busy)?;
    spawn_reserved(permit, work)
}

pub(crate) fn reserve_cache() -> Option<Permit<'static>> {
    CACHE_IO.acquire()
}
pub(crate) fn dispatch_cache(
    permit: Permit<'static>,
    work: impl FnOnce() + Send + 'static,
) -> Result<(), DispatchError> {
    spawn_reserved(permit, work).map(|_| ())
}
fn spawn_reserved(
    permit: Permit<'static>,
    work: impl FnOnce() + Send + 'static,
) -> Result<std::thread::JoinHandle<()>, DispatchError> {
    std::thread::Builder::new()
        .name("seecut-project-io".into())
        .spawn(move || {
            let _permit = permit;
            work();
        })
        .map_err(DispatchError::Spawn)
}

#[derive(Clone)]
pub(crate) struct Request {
    pub(crate) id: u64,
    pub(crate) generation: u64,
    pub(crate) revision: u64,
    pub(crate) target: String,
    pub(crate) cancelled: Arc<AtomicBool>,
}
impl Request {
    pub(crate) fn new(id: u64, generation: u64, revision: u64, target: String) -> Self {
        Self {
            id,
            generation,
            revision,
            target,
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }
    pub(crate) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
    pub(crate) fn valid(&self, id: u64, generation: u64, revision: u64) -> bool {
        !self.cancelled.load(Ordering::Acquire)
            && self.id == id
            && self.generation == generation
            && self.revision == revision
    }
    pub(crate) fn check(&self) -> Result<(), String> {
        if self.cancelled.load(Ordering::Acquire) {
            Err("Project opening cancelled".into())
        } else {
            Ok(())
        }
    }
}

/// Consumes only the current reply. Dropping an obsolete result also releases
/// the prepared Session's owner, while a newer pending request stays intact.
pub(crate) enum Completion<T, E> {
    Obsolete,
    Changed,
    SaveSuperseded,
    Failed(E),
    Ready(T),
}
pub(crate) fn complete<T, E>(
    pending: &mut Option<Request>,
    id: u64,
    generation: u64,
    revision: u64,
    save_latest: bool,
    unsettled_edit: bool,
    result: Result<T, E>,
) -> Completion<T, E> {
    let Some(request) = pending.as_ref() else {
        return Completion::Obsolete;
    };
    if request.id != id {
        return Completion::Obsolete;
    }
    let current = request.valid(id, generation, revision) && !unsettled_edit;
    let request = pending.take().expect("checked");
    if !current {
        request.cancel();
        return Completion::Changed;
    }
    if !save_latest {
        return Completion::SaveSuperseded;
    }
    match result {
        Ok(prepared) => Completion::Ready(prepared),
        Err(error) => Completion::Failed(error),
    }
}

/// Binds one pending asset batch to the open that the user actually chose.
#[derive(Clone)]
pub(crate) struct HandoffBinding {
    target: Option<String>,
    request: Option<u64>,
}
impl HandoffBinding {
    pub(crate) fn existing(path: String) -> Self {
        Self {
            target: Some(path),
            request: None,
        }
    }
    pub(crate) fn create() -> Self {
        Self {
            target: None,
            request: None,
        }
    }
    pub(crate) fn started(&mut self, id: u64, target: &str, created: bool) {
        if match self.target.as_deref() {
            Some(path) => !created && path == target,
            None => created,
        } {
            self.request = Some(id);
        }
    }
    pub(crate) fn failed(&mut self, id: u64) -> bool {
        if self.request != Some(id) {
            return false;
        }
        self.request = None;
        true
    }
    pub(crate) fn completed(&self, id: u64) -> bool {
        self.request == Some(id)
    }
}

/// Document edits and completed mask analyses both invalidate blank-mask probes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CacheStamp {
    pub(crate) generation: u64,
    pub(crate) revision: u64,
    pub(crate) mask_epoch: u64,
}

impl CacheStamp {
    pub(crate) fn store(
        self,
        current: Self,
        cache: &mut std::collections::HashMap<String, bool>,
        key: String,
        blank: bool,
    ) -> bool {
        if self != current {
            return false;
        }
        if cache.len() >= 128 {
            cache.clear();
        }
        cache.insert(key, blank);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;
    struct PreparedOwner(Arc<AtomicUsize>);
    impl Drop for PreparedOwner {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn actual_worker_keeps_capacity_after_cancel_while_caller_can_continue() {
        let capacity = Box::leak(Box::new(Capacity::new(1)));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let request = Request::new(1, 2, 3, "synthetic".into());
        let worker_request = request.clone();
        let worker = dispatch_from(capacity, move || {
            entered_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(worker_request.check().is_err());
        })
        .unwrap();
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        request.cancel();
        assert!(matches!(
            dispatch_from(capacity, || panic!("must not start")),
            Err(DispatchError::Busy)
        ));
        release_tx.send(()).unwrap();
        worker.join().unwrap();
        dispatch_from(capacity, || {}).unwrap().join().unwrap();
    }

    #[test]
    fn real_completion_consumer_adopts_b_once_and_drops_late_a_owner() {
        let capacity = Box::leak(Box::new(Capacity::new(2)));
        let dropped_a = Arc::new(AtomicUsize::new(0));
        let dropped_b = Arc::new(AtomicUsize::new(0));
        let (ready_tx, ready_rx) = mpsc::channel();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let a = Request::new(1, 10, 20, "A".into());
        let mut pending = Some(a.clone());
        let a_tx = ready_tx.clone();
        let a_owner = PreparedOwner(dropped_a.clone());
        let a_worker = dispatch_from(capacity, move || {
            entered_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            a_tx.send((1, a_owner)).unwrap();
        })
        .unwrap();
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        pending.take().unwrap().cancel();
        pending = Some(Request::new(2, 10, 20, "B".into()));
        let b_owner = PreparedOwner(dropped_b.clone());
        let b_worker = dispatch_from(capacity, move || {
            ready_tx.send((2, b_owner)).unwrap();
        })
        .unwrap();
        let (id, owner) = ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(id, 2);
        let adopted = match complete(&mut pending, id, 10, 20, true, false, Ok::<_, ()>(owner)) {
            Completion::Ready(owner) => owner,
            _ => panic!("B must adopt"),
        };
        assert!(pending.is_none());
        release_tx.send(()).unwrap();
        let (id, owner) = ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(
            complete(&mut pending, id, 11, 21, true, false, Ok::<_, ()>(owner)),
            Completion::Obsolete
        ));
        assert_eq!(dropped_a.load(Ordering::SeqCst), 1);
        assert_eq!(dropped_b.load(Ordering::SeqCst), 0);
        drop(adopted);
        a_worker.join().unwrap();
        b_worker.join().unwrap();
    }

    #[test]
    fn rejected_completion_releases_prepared_owner_without_adoption() {
        for (revision, latest, unsettled) in
            [(21, true, false), (20, false, false), (20, true, true)]
        {
            let dropped = Arc::new(AtomicUsize::new(0));
            let mut pending = Some(Request::new(1, 10, 20, "new target".into()));
            let result = complete(
                &mut pending,
                1,
                10,
                revision,
                latest,
                unsettled,
                Ok::<_, ()>(PreparedOwner(dropped.clone())),
            );
            assert!(!matches!(result, Completion::Ready(_)));
            assert!(pending.is_none());
            assert_eq!(dropped.load(Ordering::SeqCst), 1);
        }
        let mut pending = Some(Request::new(1, 10, 20, "target".into()));
        assert!(matches!(
            complete(
                &mut pending,
                1,
                10,
                20,
                true,
                false,
                Err::<(), _>("save failed")
            ),
            Completion::Failed("save failed")
        ));
        assert!(pending.is_none());
    }

    #[test]
    fn cancel_and_timeout_late_completion_leave_newer_request_untouched() {
        let cancelled = Request::new(1, 10, 20, "A".into());
        cancelled.cancel();
        let mut pending = Some(cancelled);
        assert!(matches!(
            complete(&mut pending, 1, 10, 20, true, false, Ok::<_, ()>(())),
            Completion::Changed
        ));
        pending = Some(Request::new(2, 10, 20, "B".into()));
        assert!(matches!(
            complete(&mut pending, 1, 10, 20, true, false, Ok::<_, ()>(())),
            Completion::Obsolete
        ));
        assert_eq!(pending.as_ref().unwrap().id, 2);
    }

    #[test]
    fn late_blank_reply_cannot_override_reanalysis_or_undo_at_same_frame() {
        let first = CacheStamp {
            generation: 1,
            revision: 4,
            mask_epoch: 8,
        };
        let (tx, rx) = mpsc::channel();
        let worker = std::thread::spawn(move || tx.send((first, true)).unwrap());
        let mut cache = std::collections::HashMap::new();
        for current in [
            CacheStamp {
                mask_epoch: 9,
                ..first
            },
            CacheStamp {
                revision: 5,
                ..first
            },
        ] {
            assert!(current.store(current, &mut cache, "frame".into(), false));
            assert!(!first.store(current, &mut cache, "frame".into(), true));
            assert_eq!(cache.get("frame"), Some(&false));
        }
        let (old, blank) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let current = CacheStamp {
            generation: 2,
            ..first
        };
        assert!(!old.store(current, &mut cache, "frame".into(), blank));
        worker.join().unwrap();
    }
}
