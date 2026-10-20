//! 原导出任务的即时撤销入口，不借用 Studio、Bridge 或 Cloud。
//! 这里只保留执行中的 Revoker；原结果和输出证明仍归原 Receipts。

use std::cell::{Cell, RefCell};

use concat_host::export_publication::{PublishRevocation, PublishRevoker};

struct Entry {
    registration: u64,
    permit: u64,
    client: String,
    assistant_session: Option<String>,
    revoker: PublishRevoker,
}

thread_local! {
    static ENTRIES: RefCell<Vec<Entry>> = const { RefCell::new(Vec::new()) };
    static NEXT: Cell<u64> = const { Cell::new(0) };
}

/// UI 线程在 worker 入队前登记；容量或计数耗尽时不接受新任务。
pub(crate) fn register(
    permit: u64,
    client: &str,
    assistant_session: Option<&str>,
    revoker: PublishRevoker,
) -> Option<u64> {
    let registration = NEXT.get().checked_add(1)?;
    ENTRIES.with(|entries| {
        let mut entries = entries.borrow_mut();
        if entries.len() >= 128 {
            return None;
        }
        entries.push(Entry {
            registration,
            permit,
            client: client.into(),
            assistant_session: assistant_session.map(str::to_owned),
            revoker,
        });
        NEXT.set(registration);
        Some(registration)
    })
}

/// 原完成回执交付时移除该任务，不影响之后登记的任务。
pub(crate) fn finish(registration: u64) {
    ENTRIES.with(|entries| {
        entries
            .borrow_mut()
            .retain(|entry| entry.registration != registration);
    });
}

/// 仅真实生命周期/权限事件调用。普通编辑和选择不调用此入口。
pub(crate) fn invalidate(reason: PublishRevocation) {
    let entries = ENTRIES.with(|entries| std::mem::take(&mut *entries.borrow_mut()));
    for entry in entries {
        // Publishing 已先胜出时，Revoker 保留真实状态且不改写取消信号。
        let _ = entry.revoker.revoke(reason);
    }
}

/// Task/Agent 会话只撤销发起客户端自己的任务。
pub(crate) fn revoke_client(client: &str, reason: PublishRevocation) {
    ENTRIES.with(|entries| {
        entries.borrow_mut().retain(|entry| {
            if entry.client == client {
                let _ = entry.revoker.revoke(reason);
                false
            } else {
                true
            }
        });
    });
}

pub(crate) fn revoke_permit(permit: u64) {
    ENTRIES.with(|entries| {
        entries.borrow_mut().retain(|entry| {
            if entry.permit == permit {
                let _ = entry.revoker.revoke(PublishRevocation::Revoked);
                false
            } else {
                true
            }
        });
    });
}

/// 控制器可能属于停放的旧会话，只能撤销它自己的原任务。
pub(crate) fn revoke_session(session: &str, reason: PublishRevocation) {
    if session.is_empty() {
        return;
    }
    ENTRIES.with(|entries| {
        entries.borrow_mut().retain(|entry| {
            if entry.assistant_session.as_deref() == Some(session) {
                let _ = entry.revoker.revoke(reason);
                false
            } else {
                true
            }
        });
    });
}

pub(crate) fn document_will_change() {
    invalidate(PublishRevocation::DocumentChanged);
}

#[cfg(test)]
mod tests {
    use super::*;
    use concat_host::export_publication::{PublishRefusal, PublishState, new_publish_gate};
    use std::sync::{Arc, atomic::AtomicBool};

    #[test]
    fn source_revocation_does_not_wait_for_a_studio_borrow() {
        let borrowed_ui = RefCell::new(());
        let _busy = borrowed_ui.borrow_mut();
        let (permit, revoker) = new_publish_gate(Arc::new(AtomicBool::new(false)), None);
        let id = register(1, "assistant", Some("session"), revoker.clone()).unwrap();
        invalidate(PublishRevocation::SessionChanged);
        assert_eq!(
            permit.try_claim().unwrap_err(),
            PublishRefusal::Revoked(PublishRevocation::SessionChanged)
        );
        finish(id);
    }

    #[test]
    fn ordinary_selection_and_save_fence_do_not_revoke_frozen_exports() {
        let (permit, revoker) = new_publish_gate(Arc::new(AtomicBool::new(false)), None);
        let id = register(2, "assistant", Some("session"), revoker.clone()).unwrap();
        let mut selection = crate::clip_save_fence::Selection::<Vec<String>>::default();
        selection.push("another-clip".into());
        selection.clear();
        crate::clip_save_fence::before_change();
        assert_eq!(revoker.state(), PublishState::Pending);
        let _claim = permit.try_claim().unwrap();
        finish(id);
    }

    #[test]
    fn revoking_an_original_permit_does_not_touch_another_registration() {
        let (first, first_revoker) = new_publish_gate(Arc::new(AtomicBool::new(false)), None);
        let (second, second_revoker) = new_publish_gate(Arc::new(AtomicBool::new(false)), None);
        let old = register(3, "assistant", Some("session"), first_revoker.clone()).unwrap();
        let current = register(4, "assistant", Some("session"), second_revoker.clone()).unwrap();
        revoke_permit(3);
        assert_eq!(
            first.try_claim().unwrap_err(),
            PublishRefusal::Revoked(PublishRevocation::Revoked)
        );
        finish(old);
        assert_eq!(second_revoker.state(), PublishState::Pending);
        let _claim = second.try_claim().unwrap();
        finish(current);
    }

    #[test]
    fn lifecycle_revocation_after_claim_preserves_publication_and_original_flag() {
        let cancel = Arc::new(AtomicBool::new(false));
        let (permit, revoker) = new_publish_gate(Arc::clone(&cancel), None);
        let id = register(5, "assistant", Some("session"), revoker.clone()).unwrap();
        let _claim = permit.try_claim().unwrap();
        invalidate(PublishRevocation::AccountChanged);
        assert_eq!(revoker.state(), PublishState::Publishing);
        assert!(!cancel.load(std::sync::atomic::Ordering::Acquire));
        finish(id);
    }
}
