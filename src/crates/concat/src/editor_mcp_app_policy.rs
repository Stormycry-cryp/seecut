// SPDX-License-Identifier: AGPL-3.0-or-later
//! In-memory App read grants. No deadline, persistence, document permission or writes.
use std::collections::HashMap;

#[derive(Clone, Eq, PartialEq)]
pub(super) struct Authority {
    pub instance_id: String,
    pub account_epoch: u64,
    pub account_scope: String,
    pub app_session_id: String,
}
struct Grant {
    authority: Authority,
    token: String,
    media: bool,
}
#[derive(Default)]
pub(super) struct Grants {
    items: HashMap<String, Grant>,
}
impl Grants {
    pub fn grant(&mut self, client: &str, authority: Authority, media: bool) -> Result<(), ()> {
        if client.is_empty()
            || client.len() > 64
            || client.chars().any(char::is_control)
            || (!self.items.contains_key(client) && self.items.len() >= 32)
        {
            return Err(());
        }
        self.items.insert(
            client.into(),
            Grant {
                authority,
                token: uuid::Uuid::new_v4().to_string(),
                media,
            },
        );
        Ok(())
    }
    pub fn token(&self, client: &str, authority: &Authority) -> Option<String> {
        self.items
            .get(client)
            .filter(|grant| &grant.authority == authority)
            .map(|grant| grant.token.clone())
    }
    pub fn authorized(
        &self,
        client: &str,
        token: &str,
        authority: &Authority,
        media: bool,
    ) -> bool {
        self.items.get(client).is_some_and(|grant| {
            grant.authority == *authority && grant.token == token && (!media || grant.media)
        })
    }
    pub fn has(&self, client: &str) -> bool {
        self.items.contains_key(client)
    }
    pub fn media(&self, client: &str, authority: &Authority) -> bool {
        self.items
            .get(client)
            .is_some_and(|grant| grant.authority == *authority && grant.media)
    }
    pub fn revoke(&mut self, client: &str) {
        self.items.remove(client);
    }
    pub fn clear(&mut self) {
        self.items.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn authority() -> Authority {
        Authority {
            instance_id: "i".into(),
            account_epoch: 1,
            account_scope: "a".repeat(64),
            app_session_id: "s".into(),
        }
    }
    #[test]
    fn exact_grant_cannot_cross_account_epoch_session_instance_or_client() {
        let mut grants = Grants::default();
        let a = authority();
        grants.grant("c", a.clone(), true).unwrap();
        let t = grants.token("c", &a).unwrap();
        assert!(grants.authorized("c", &t, &a, true));
        assert!(grants.media("c", &a));
        assert!(!grants.authorized("other", &t, &a, false));
        for n in 0..4 {
            let mut changed = a.clone();
            match n {
                0 => changed.instance_id = "other".into(),
                1 => changed.account_epoch += 1,
                2 => changed.account_scope = "b".repeat(64),
                _ => changed.app_session_id = "new".into(),
            }
            assert!(!grants.authorized("c", &t, &changed, false));
        }
    }
    #[test]
    fn explicit_revoke_and_regrant_invalidate_original_preview_request() {
        let mut grants = Grants::default();
        let a = authority();
        grants.grant("c", a.clone(), true).unwrap();
        let t = grants.token("c", &a).unwrap();
        grants.revoke("c");
        assert!(!grants.authorized("c", &t, &a, true));
        grants.grant("c", a.clone(), true).unwrap();
        assert!(!grants.authorized("c", &t, &a, true));
    }
    #[test]
    fn media_is_separate_and_navigation_is_not_authority() {
        let mut grants = Grants::default();
        let a = authority();
        grants.grant("c", a.clone(), false).unwrap();
        let t = grants.token("c", &a).unwrap();
        assert!(grants.authorized("c", &t, &a, false));
        assert!(!grants.authorized("c", &t, &a, true));
        // No time or page state is in the grant. Current user-session authority
        // remains until explicit revoke/account change; queries still freeze view.
        assert!(grants.authorized("c", &t, &a, false));
        grants.clear();
        assert!(!grants.has("c"));
    }
}
