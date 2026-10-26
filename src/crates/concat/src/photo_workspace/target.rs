//! Explicit UI target, independent of native focus and managed process lifetime.
use seecut_photo_host::Uuid;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct ManagedSource {
    pub(crate) bind_id: Uuid,
    pub(crate) epoch: u64,
    pub(crate) instance: Uuid,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct PhotoTarget {
    pub(crate) managed: ManagedSource,
    pub(crate) epoch: u64,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum AgentTarget {
    Parent,
    Photo {
        bind_id: Uuid,
        managed_epoch: u64,
        instance: Option<Uuid>,
    },
}
pub(super) struct TargetState {
    target: AgentTarget,
    epoch: u64,
    exhausted: bool,
}
impl Default for TargetState {
    fn default() -> Self {
        Self {
            target: AgentTarget::Parent,
            epoch: 0,
            exhausted: false,
        }
    }
}
impl TargetState {
    pub(super) fn is_photo(&self) -> bool {
        matches!(self.target, AgentTarget::Photo { .. })
    }
    fn advance(&mut self) -> bool {
        match self.epoch.checked_add(1).filter(|_| !self.exhausted) {
            Some(next) => {
                self.epoch = next;
                true
            }
            None => {
                self.exhausted = true;
                false
            }
        }
    }
    /// Actual committed parent navigation only; a cancelled attempt never calls it.
    pub(super) fn parent(&mut self) -> bool {
        self.target = AgentTarget::Parent;
        self.advance()
    }
    pub(super) fn photo(
        &mut self,
        bind_id: Uuid,
        managed_epoch: u64,
        instance: Option<Uuid>,
    ) -> bool {
        let next = AgentTarget::Photo {
            bind_id,
            managed_epoch,
            instance,
        };
        if self.target == next {
            return !self.exhausted;
        }
        self.target = next;
        self.advance()
    }
    /// Account changes invalidate even an unchanged visual target.
    pub(super) fn invalidate(&mut self) {
        self.advance();
    }
    /// A late Ready must match the pending target's original managed generation.
    pub(super) fn ready(&mut self, source: ManagedSource) -> bool {
        if self.exhausted {
            return false;
        }
        match &mut self.target {
            AgentTarget::Photo {
                bind_id,
                managed_epoch,
                instance,
            } if *bind_id == source.bind_id
                && *managed_epoch == source.epoch
                && instance.is_none() =>
            {
                *instance = Some(source.instance);
                true
            }
            _ => false,
        }
    }
    pub(super) fn current(&self, managed: Option<ManagedSource>) -> Option<PhotoTarget> {
        if self.exhausted {
            return None;
        }
        let source = managed?;
        match self.target {
            AgentTarget::Photo {
                bind_id,
                managed_epoch,
                instance: Some(instance),
            } if bind_id == source.bind_id
                && managed_epoch == source.epoch
                && instance == source.instance =>
            {
                Some(PhotoTarget {
                    managed: source,
                    epoch: self.epoch,
                })
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source() -> ManagedSource {
        ManagedSource {
            bind_id: Uuid::new_v4(),
            epoch: 4,
            instance: Uuid::new_v4(),
        }
    }
    #[test]
    fn explicit_target_survives_agent_open_but_not_parent_navigation() {
        let source = source();
        let mut target = TargetState::default();
        assert!(!target.is_photo());
        assert!(target.photo(source.bind_id, source.epoch, Some(source.instance)));
        let first = target.current(Some(source)).unwrap();
        // Reading/opening the Agent has no target mutation API.
        assert!(target.current(Some(source)) == Some(first));
        assert!(target.parent());
        assert!(!target.is_photo());
        assert!(target.current(Some(source)).is_none());
        assert!(target.photo(source.bind_id, source.epoch, Some(source.instance)));
        assert!(target.current(Some(source)).unwrap().epoch > first.epoch);
    }
    #[test]
    fn late_ready_never_retargets_parent_or_replaced_child() {
        let source = source();
        let mut target = TargetState::default();
        target.photo(source.bind_id, source.epoch, None);
        assert!(target.is_photo() && target.current(Some(source)).is_none());
        target.parent();
        assert!(!target.ready(source));
        assert!(!target.is_photo());
        target.photo(source.bind_id, source.epoch + 1, None);
        assert!(!target.ready(source));
        let next = ManagedSource {
            epoch: source.epoch + 1,
            ..source
        };
        assert!(target.ready(next));
        assert!(target.current(Some(next)).is_some());
    }
    #[test]
    fn same_target_refresh_preserves_epoch_account_change_and_overflow_do_not() {
        let source = source();
        let mut target = TargetState::default();
        target.photo(source.bind_id, source.epoch, Some(source.instance));
        let first = target.current(Some(source)).unwrap();
        target.photo(source.bind_id, source.epoch, Some(source.instance));
        assert!(target.current(Some(source)) == Some(first));
        target.invalidate();
        assert!(target.current(Some(source)).unwrap().epoch > first.epoch);
        target.epoch = u64::MAX;
        target.invalidate();
        assert!(target.current(Some(source)).is_none());
        target.parent();
        target.photo(source.bind_id, source.epoch, Some(source.instance));
        assert!(target.current(Some(source)).is_none());
    }
}
