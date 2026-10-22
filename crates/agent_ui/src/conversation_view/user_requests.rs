use acp_thread::{ElicitationEntryId, PermissionRequestId};
use agent_client_protocol::schema::v2 as acp_v2;
use collections::IndexMap;

/// A request for the user's input from one session of a conversation.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum UserRequestRef {
    Permission {
        session_id: acp_v2::SessionId,
        request_id: PermissionRequestId,
    },
    Elicitation {
        session_id: acp_v2::SessionId,
        elicitation_id: ElicitationEntryId,
    },
}

impl UserRequestRef {
    pub fn session_id(&self) -> &acp_v2::SessionId {
        match self {
            Self::Permission { session_id, .. } | Self::Elicitation { session_id, .. } => {
                session_id
            }
        }
    }
}

/// How a request joins the inbox.
pub(crate) enum RequestGrouping {
    /// Shown on its own, or together with identical requests from other
    /// sessions that share the same key, so that one answer covers them all.
    Single { merge_key: Option<String> },
    /// Questions pending from the same session share one item.
    Questions,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum InboxItemId {
    Request(u64),
    Questions(acp_v2::SessionId),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct InboxItem {
    pub id: InboxItemId,
    /// In arrival order.
    pub members: Vec<UserRequestRef>,
}

struct QueuedRequest {
    seq: u64,
    item: InboxItemId,
    merge_key: Option<String>,
}

/// Every pending request for the user's input across a conversation's
/// sessions, kept in arrival order and grouped into inbox items.
///
/// The focused item only changes when the user moves focus or when it's
/// resolved; new arrivals are queued behind it.
#[derive(Default)]
pub(crate) struct UserRequestQueue {
    next_seq: u64,
    requests: IndexMap<UserRequestRef, QueuedRequest>,
    focused: Option<InboxItemId>,
}

impl UserRequestQueue {
    /// Returns whether the request was new.
    pub fn insert(&mut self, request: UserRequestRef, grouping: RequestGrouping) -> bool {
        if self.requests.contains_key(&request) {
            return false;
        }
        let seq = self.next_seq;
        self.next_seq += 1;
        let (item, merge_key) = match grouping {
            RequestGrouping::Questions => {
                (InboxItemId::Questions(request.session_id().clone()), None)
            }
            RequestGrouping::Single { merge_key } => {
                let duplicate_of = merge_key.as_ref().and_then(|merge_key| {
                    self.requests
                        .values()
                        .find(|queued| {
                            queued.merge_key.as_ref() == Some(merge_key)
                                && !self.item_has_session(&queued.item, request.session_id())
                        })
                        .map(|queued| queued.item.clone())
                });
                (duplicate_of.unwrap_or(InboxItemId::Request(seq)), merge_key)
            }
        };
        if self.focused.is_none() {
            self.focused = Some(item.clone());
        }
        self.requests.insert(
            request,
            QueuedRequest {
                seq,
                item,
                merge_key,
            },
        );
        true
    }

    /// Returns whether resolving this request moved focus to another item.
    pub fn remove(
        &mut self,
        request: &UserRequestRef,
        in_viewer_subtree: impl Fn(&acp_v2::SessionId) -> bool,
    ) -> bool {
        let previous_order = self
            .items(&in_viewer_subtree)
            .into_iter()
            .map(|item| item.id)
            .collect::<Vec<_>>();
        if self.requests.shift_remove(request).is_none() {
            return false;
        }
        let Some(focused) = self.focused.clone() else {
            return false;
        };
        if self.contains_item(&focused) {
            return false;
        }
        let position = previous_order
            .iter()
            .position(|id| id == &focused)
            .unwrap_or(0);
        let following = previous_order.iter().skip(position + 1);
        let preceding = previous_order.iter().take(position).rev();
        self.focused = following
            .chain(preceding)
            .find(|id| self.contains_item(id))
            .cloned();
        self.focused.is_some()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.requests.is_empty()
    }

    /// Every pending request, in arrival order.
    pub fn requests(&self) -> impl Iterator<Item = &UserRequestRef> {
        self.requests.keys()
    }

    pub fn item_for_request(&self, request: &UserRequestRef) -> Option<InboxItemId> {
        self.requests.get(request).map(|queued| queued.item.clone())
    }

    /// The requests answered together as `item`, in arrival order.
    pub fn members(&self, item: &InboxItemId) -> Vec<UserRequestRef> {
        self.requests
            .iter()
            .filter(|(_, queued)| &queued.item == item)
            .map(|(request, _)| request.clone())
            .collect()
    }

    pub fn focused_id(&self) -> Option<&InboxItemId> {
        self.focused.as_ref()
    }

    /// Items for sessions in the viewer's subtree come first, then everything
    /// else in arrival order.
    pub fn items(&self, in_viewer_subtree: impl Fn(&acp_v2::SessionId) -> bool) -> Vec<InboxItem> {
        let mut items: Vec<(u64, InboxItem)> = Vec::new();
        for (request, queued) in &self.requests {
            if let Some((_, item)) = items.iter_mut().find(|(_, item)| item.id == queued.item) {
                item.members.push(request.clone());
            } else {
                items.push((
                    queued.seq,
                    InboxItem {
                        id: queued.item.clone(),
                        members: vec![request.clone()],
                    },
                ));
            }
        }
        items.sort_by_key(|(seq, item)| {
            let in_subtree = item
                .members
                .iter()
                .any(|member| in_viewer_subtree(member.session_id()));
            (!in_subtree, *seq)
        });
        items.into_iter().map(|(_, item)| item).collect()
    }

    #[cfg(test)]
    pub fn focused_item(
        &self,
        in_viewer_subtree: impl Fn(&acp_v2::SessionId) -> bool,
    ) -> Option<InboxItem> {
        let items = self.items(in_viewer_subtree);
        let focused = items
            .iter()
            .position(|item| Some(&item.id) == self.focused.as_ref())
            .unwrap_or(0);
        items.into_iter().nth(focused)
    }

    pub fn focus(&mut self, id: &InboxItemId) -> bool {
        if !self.contains_item(id) {
            return false;
        }
        self.focused = Some(id.clone());
        true
    }

    /// Moves focus by `offset` items, wrapping around at either end.
    pub fn cycle_focus(
        &mut self,
        offset: isize,
        in_viewer_subtree: impl Fn(&acp_v2::SessionId) -> bool,
    ) {
        let items = self.items(in_viewer_subtree);
        if items.is_empty() {
            return;
        }
        let current = items
            .iter()
            .position(|item| Some(&item.id) == self.focused.as_ref())
            .unwrap_or(0) as isize;
        let next = (current + offset).rem_euclid(items.len() as isize) as usize;
        self.focused = items.get(next).map(|item| item.id.clone());
    }

    /// When the viewer changes, focuses the first item in its subtree unless
    /// the focused item is already there. Returns whether focus moved.
    pub fn focus_viewer_subtree(
        &mut self,
        in_viewer_subtree: impl Fn(&acp_v2::SessionId) -> bool,
    ) -> bool {
        let items = self.items(&in_viewer_subtree);
        let in_subtree = |item: &InboxItem| {
            item.members
                .iter()
                .any(|member| in_viewer_subtree(member.session_id()))
        };
        if items
            .iter()
            .any(|item| Some(&item.id) == self.focused.as_ref() && in_subtree(item))
        {
            return false;
        }
        let Some(first) = items.into_iter().find(|item| in_subtree(item)) else {
            return false;
        };
        self.focused = Some(first.id);
        true
    }

    fn contains_item(&self, id: &InboxItemId) -> bool {
        self.requests.values().any(|queued| &queued.item == id)
    }

    fn item_has_session(&self, item: &InboxItemId, session_id: &acp_v2::SessionId) -> bool {
        self.requests
            .iter()
            .any(|(request, queued)| &queued.item == item && request.session_id() == session_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn session(id: &str) -> acp_v2::SessionId {
        acp_v2::SessionId::new(id)
    }

    fn question(session_id: &str, id: &str) -> UserRequestRef {
        UserRequestRef::Elicitation {
            session_id: session(session_id),
            elicitation_id: ElicitationEntryId(id.into()),
        }
    }

    fn permission(session_id: &str) -> UserRequestRef {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        UserRequestRef::Permission {
            session_id: session(session_id),
            request_id: serde_json::from_value(serde_json::json!(format!(
                "00000000-0000-0000-0000-{id:012}"
            )))
            .expect("permission request id"),
        }
    }

    fn single(merge_key: Option<&str>) -> RequestGrouping {
        RequestGrouping::Single {
            merge_key: merge_key.map(ToOwned::to_owned),
        }
    }

    fn everywhere(_: &acp_v2::SessionId) -> bool {
        true
    }

    fn ids(queue: &UserRequestQueue) -> Vec<InboxItemId> {
        queue
            .items(everywhere)
            .into_iter()
            .map(|item| item.id)
            .collect()
    }

    #[test]
    fn test_items_keep_arrival_order_across_sessions() {
        let mut queue = UserRequestQueue::default();
        let root_first = permission("root");
        let child = permission("child");
        let root_second = permission("root");
        for request in [&root_first, &child, &root_second] {
            assert!(queue.insert(request.clone(), single(None)));
        }
        assert!(!queue.insert(child.clone(), single(None)));

        let members = queue
            .items(everywhere)
            .into_iter()
            .map(|item| item.members)
            .collect::<Vec<_>>();
        assert_eq!(
            members,
            vec![vec![root_first], vec![child], vec![root_second]]
        );
    }

    #[test]
    fn test_identical_requests_from_different_sessions_merge() {
        let mut queue = UserRequestQueue::default();
        let first = permission("a");
        let same_session = permission("a");
        let other_session = permission("b");
        let different = permission("c");
        queue.insert(first.clone(), single(Some("cargo test")));
        queue.insert(same_session.clone(), single(Some("cargo test")));
        queue.insert(other_session.clone(), single(Some("cargo test")));
        queue.insert(different.clone(), single(Some("rm -rf target")));

        let members = queue
            .items(everywhere)
            .into_iter()
            .map(|item| item.members)
            .collect::<Vec<_>>();
        assert_eq!(
            members,
            vec![
                vec![first, other_session],
                vec![same_session],
                vec![different]
            ]
        );
    }

    #[test]
    fn test_questions_from_one_session_share_an_item() {
        let mut queue = UserRequestQueue::default();
        queue.insert(question("a", "1"), RequestGrouping::Questions);
        queue.insert(permission("b"), single(None));
        queue.insert(question("a", "2"), RequestGrouping::Questions);
        queue.insert(question("b", "3"), RequestGrouping::Questions);

        let items = queue.items(everywhere);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].id, InboxItemId::Questions(session("a")));
        assert_eq!(
            items[0].members,
            vec![question("a", "1"), question("a", "2")]
        );
        assert_eq!(items[2].id, InboxItemId::Questions(session("b")));
    }

    #[test]
    fn test_focus_stays_put_on_arrival_and_advances_on_resolution() {
        let mut queue = UserRequestQueue::default();
        let first = permission("a");
        let second = permission("b");
        let third = permission("c");
        queue.insert(first.clone(), single(None));
        let first_id = queue.item_for_request(&first).expect("first item");
        assert_eq!(
            queue.focused_item(everywhere).map(|item| item.id),
            Some(first_id)
        );

        queue.insert(second.clone(), single(None));
        queue.insert(third.clone(), single(None));
        let [_, second_id, third_id] = ids(&queue).try_into().expect("three items");
        assert_eq!(
            queue.focused_item(everywhere).map(|item| item.members),
            Some(vec![first.clone()]),
            "new arrivals don't take focus"
        );

        assert!(queue.focus(&second_id));
        assert!(
            !queue.remove(&first, everywhere),
            "unfocused items resolve quietly"
        );
        assert!(queue.remove(&second, everywhere));
        assert_eq!(
            queue.focused_item(everywhere).map(|item| item.id),
            Some(third_id),
            "focus moves to the item after the resolved one"
        );
        assert!(!queue.remove(&third, everywhere), "nothing left to focus");
        assert!(queue.is_empty());
    }

    #[test]
    fn test_resolving_the_last_item_focuses_the_one_before_it() {
        let mut queue = UserRequestQueue::default();
        let last = permission("b");
        queue.insert(permission("a"), single(None));
        queue.insert(last.clone(), single(None));
        let [first_id, last_id] = ids(&queue).try_into().expect("two items");

        queue.focus(&last_id);
        assert!(queue.remove(&last, everywhere));
        assert_eq!(
            queue.focused_item(everywhere).map(|item| item.id),
            Some(first_id)
        );
    }

    #[test]
    fn test_viewer_subtree_comes_first() {
        let mut queue = UserRequestQueue::default();
        let root = permission("root");
        let child = permission("child");
        queue.insert(root.clone(), single(None));
        queue.insert(child.clone(), single(None));
        let viewing_child = |session_id: &acp_v2::SessionId| session_id == &session("child");

        let order = queue
            .items(viewing_child)
            .into_iter()
            .map(|item| item.members)
            .collect::<Vec<_>>();
        assert_eq!(order, vec![vec![child.clone()], vec![root]]);

        assert!(queue.focus_viewer_subtree(viewing_child));
        assert_eq!(
            queue.focused_item(viewing_child).map(|item| item.members),
            Some(vec![child])
        );
        assert!(
            !queue.focus_viewer_subtree(viewing_child),
            "focus already in the subtree"
        );
    }

    #[test]
    fn test_cycling_focus_wraps_around() {
        let mut queue = UserRequestQueue::default();
        for session_id in ["a", "b", "c"] {
            queue.insert(permission(session_id), single(None));
        }
        let [first, second, third] = ids(&queue).try_into().expect("three items");

        queue.cycle_focus(-1, everywhere);
        assert_eq!(
            queue.focused_item(everywhere).map(|item| item.id),
            Some(third)
        );
        queue.cycle_focus(1, everywhere);
        assert_eq!(
            queue.focused_item(everywhere).map(|item| item.id),
            Some(first)
        );
        queue.cycle_focus(1, everywhere);
        assert_eq!(
            queue.focused_item(everywhere).map(|item| item.id),
            Some(second)
        );
    }
}
