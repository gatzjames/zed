use crate::{
    AcpThread, AcpThreadEvent, AgentThreadEntry, ElicitationStatus, MessageIdentity, MessageKind,
    SubagentSessionInfo, content,
};
use action_log::ActionLog;
use agent_client_protocol::schema::{MaybeUndefined, v1 as acp_v1, v2 as acp_v2};
use anyhow::Result;
use gpui::{AppContext as _, Context, Entity, SharedString};
use std::ops::Range;

/// Display metadata and parent-reported state for a session that an ACP agent
/// announced as one of its subagents.
#[derive(Clone, Debug, Default)]
pub struct SubagentInfo {
    pub title: Option<SharedString>,
    pub description: Option<SharedString>,
    pub can_cancel: bool,
    pub reported_state: Option<acp_v2::StateUpdate>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentRunStatus {
    /// Announced, but no state has been reported yet.
    Starting,
    Running,
    WaitingForUser,
    Done,
    Failed,
    Cancelled,
    /// The agent no longer knows the subagent's state, e.g. after a disconnect.
    Unknown,
}

impl AgentRunStatus {
    pub fn is_active(self) -> bool {
        matches!(self, Self::Starting | Self::Running | Self::WaitingForUser)
    }
}

impl AcpThread {
    pub fn subagent_info(&self) -> Option<&SubagentInfo> {
        self.subagent.as_ref()
    }

    pub fn subagent_thread(&self, session_id: &acp_v2::SessionId) -> Option<&Entity<AcpThread>> {
        self.subagents.get(session_id)
    }

    pub fn subagent_threads(&self) -> impl Iterator<Item = &Entity<AcpThread>> {
        self.subagents.values()
    }

    /// Returns the thread for a child the agent announced, creating it on first sight.
    pub fn spawn_reported_subagent(
        &mut self,
        session_id: acp_v2::SessionId,
        cx: &mut Context<Self>,
    ) -> Entity<AcpThread> {
        if let Some(existing) = self.subagents.get(&session_id) {
            return existing.clone();
        }
        let parent_session_id = self.session_id().clone();
        let connection = self.connection.clone();
        let project = self.project.clone();
        let work_dirs = self.work_dirs().cloned();
        let prompt_capabilities = self.prompt_capabilities.clone();
        let parent_action_log = self.action_log.clone();
        let action_log =
            cx.new(|_| ActionLog::new(project.clone()).with_linked_action_log(parent_action_log));
        let child = cx.new(|cx| {
            let mut child = AcpThread::new(
                Some(parent_session_id),
                None,
                work_dirs,
                connection,
                project,
                action_log,
                session_id.clone(),
                watch::Receiver::constant(prompt_capabilities),
                cx,
            );
            child.subagent = Some(SubagentInfo::default());
            child
        });
        self.subagents.insert(session_id.clone(), child.clone());
        cx.emit(AcpThreadEvent::SubagentSpawned(session_id));
        child
    }

    /// Applies a `subagent_update` patch to this child session.
    pub fn apply_subagent_update(
        &mut self,
        update: acp_v2::SubagentUpdate,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        anyhow::ensure!(
            &update.session_id == self.session_id(),
            "Subagent update belongs to a different session"
        );
        let info = self.subagent.get_or_insert_with(SubagentInfo::default);
        let title_changed = !update.title.is_undefined();
        if let Some(title) = patch_value(update.title) {
            info.title = title.map(SharedString::from);
        }
        if let Some(description) = patch_value(update.description) {
            info.description = description.map(SharedString::from);
        }
        if let Some(capabilities) = patch_value(update.capabilities) {
            info.can_cancel =
                capabilities.is_some_and(|capabilities| capabilities.cancel.is_some());
        }
        let state = patch_value(update.state);
        if let Some(state) = &state {
            info.reported_state = state.clone();
        }
        let title = info.title.clone();

        if title_changed {
            self.provisional_title = title;
            cx.emit(AcpThreadEvent::TitleUpdated);
        }
        match state {
            // Receipt sessions report their own state on the child's stream, so the
            // parent's copy must not advance the activity a second time.
            Some(Some(state)) if !self.uses_reported_activity() => {
                self.update_session_state(state, cx)?;
            }
            Some(_) => cx.emit(AcpThreadEvent::StatusChanged),
            None => {}
        }
        cx.notify();
        Ok(())
    }

    /// Returns the index of the entry when the message is new to this transcript.
    pub fn upsert_session_message(
        &mut self,
        message: acp_v2::SessionMessage,
        cx: &mut Context<Self>,
    ) -> Result<Option<usize>> {
        if message.sender_session_id.as_ref() == Some(self.session_id()) {
            // The delegating tool call already represents an outgoing message.
            return Ok(None);
        }
        let previous_entry_count = self.entries.len();
        let message_id = message.message_id.clone();
        self.update_keyed_message(
            MessageKind::User,
            message.message_id,
            message.content,
            message.meta,
            cx,
        )?;
        self.record_message_sender(&message_id, message.sender_session_id);
        Ok((self.entries.len() > previous_entry_count).then_some(previous_entry_count))
    }

    /// Returns the index of the entry when the chunk starts a new message.
    pub fn append_session_message_chunk(
        &mut self,
        chunk: acp_v2::SessionMessageChunk,
        cx: &mut Context<Self>,
    ) -> Result<Option<usize>> {
        if chunk.sender_session_id.as_ref() == Some(self.session_id()) {
            return Ok(None);
        }
        let previous_entry_count = self.entries.len();
        let message_id = chunk.message_id.clone();
        self.append_message_chunk(
            MessageKind::User,
            acp_v2::ContentChunk::new(chunk.content, chunk.message_id),
            cx,
        )?;
        self.record_message_sender(&message_id, chunk.sender_session_id);
        Ok((self.entries.len() > previous_entry_count).then_some(previous_entry_count))
    }

    fn record_message_sender(
        &mut self,
        message_id: &acp_v2::MessageId,
        sender_session_id: Option<acp_v2::SessionId>,
    ) {
        // Omitting the sender retains a previously known one.
        let Some(sender_session_id) = sender_session_id else {
            return;
        };
        let identity = MessageIdentity::Keyed(message_id.clone());
        for entry in self.entries.iter_mut().rev() {
            if let AgentThreadEntry::UserMessage(message) = entry
                && message.identity == identity
            {
                message.sender_session_id = Some(sender_session_id);
                return;
            }
        }
    }

    /// Links the tool call that delegated work to a subagent, or remembers the
    /// link until that tool call arrives.
    pub fn link_subagent_tool_call(
        &mut self,
        tool_call_id: acp_v2::ToolCallId,
        link: SubagentSessionInfo,
        cx: &mut Context<Self>,
    ) {
        if let Some(index) = self.index_for_tool_call(&tool_call_id)
            && let Some(AgentThreadEntry::ToolCall(call)) = self.entries.get_mut(index)
        {
            call.set_client_subagent_link(link);
            cx.emit(AcpThreadEvent::EntryUpdated(index));
        } else {
            self.pending_subagent_links.insert(tool_call_id, link);
        }
    }

    /// The entries that answer the delegated message at `start`, ending where
    /// the next delegated message begins.
    pub fn session_message_range(&self, start: usize) -> Range<usize> {
        let end = self
            .entries
            .iter()
            .enumerate()
            .skip(start.saturating_add(1))
            .find(|(_, entry)| {
                matches!(
                    entry,
                    AgentThreadEntry::UserMessage(message) if message.sender_session_id.is_some()
                )
            })
            .map_or(self.entries.len(), |(index, _)| index);
        start.min(end)..end
    }

    /// The status of an agent-announced subagent, as reported by the agent.
    pub fn run_status(&self) -> AgentRunStatus {
        if self.has_pending_user_input() {
            return AgentRunStatus::WaitingForUser;
        }
        let state = if self.received_session_state {
            self.foreground_state()
        } else {
            self.subagent
                .as_ref()
                .and_then(|subagent| subagent.reported_state.as_ref())
        };
        match state {
            None => AgentRunStatus::Starting,
            Some(acp_v2::StateUpdate::Running(_)) => AgentRunStatus::Running,
            Some(acp_v2::StateUpdate::RequiresAction(_)) => AgentRunStatus::WaitingForUser,
            Some(acp_v2::StateUpdate::Idle(idle)) => match &idle.stop_reason {
                None | Some(acp_v2::StopReason::EndTurn) => AgentRunStatus::Done,
                Some(acp_v2::StopReason::Cancelled) => AgentRunStatus::Cancelled,
                Some(_) => AgentRunStatus::Failed,
            },
            Some(_) => AgentRunStatus::Unknown,
        }
    }

    fn has_pending_user_input(&self) -> bool {
        !self.permission_requests.is_empty()
            || self
                .elicitations
                .elicitations()
                .iter()
                .any(|elicitation| matches!(elicitation.status, ElicitationStatus::Pending { .. }))
    }
}

/// `None` leaves the field unchanged; `Some(None)` clears it.
fn patch_value<T>(patch: MaybeUndefined<T>) -> Option<Option<T>> {
    match patch {
        MaybeUndefined::Undefined => None,
        MaybeUndefined::Null => Some(None),
        MaybeUndefined::Value(value) => Some(Some(value)),
    }
}

pub fn subagent_update_from_v1(update: acp_v1::SubagentUpdate) -> Result<acp_v2::SubagentUpdate> {
    // The two protocol versions share this payload's wire shape.
    Ok(serde_json::from_value(serde_json::to_value(update)?)?)
}

pub fn session_message_from_v1(message: acp_v1::SessionMessage) -> Result<acp_v2::SessionMessage> {
    let content = match message.content {
        MaybeUndefined::Undefined => MaybeUndefined::Undefined,
        MaybeUndefined::Null => MaybeUndefined::Null,
        MaybeUndefined::Value(blocks) => MaybeUndefined::Value(
            blocks
                .into_iter()
                .map(content::from_v1)
                .collect::<Result<Vec<_>>>()?,
        ),
    };
    Ok(acp_v2::SessionMessage::new(message.message_id.0)
        .sender_session_id(
            message
                .sender_session_id
                .map(|id| acp_v2::SessionId::new(id.0)),
        )
        .recipient_session_id(
            message
                .recipient_session_id
                .map(|id| acp_v2::SessionId::new(id.0)),
        )
        .content(content)
        .meta(message.meta))
}

pub fn session_message_chunk_from_v1(
    chunk: acp_v1::SessionMessageChunk,
) -> Result<acp_v2::SessionMessageChunk> {
    Ok(
        acp_v2::SessionMessageChunk::new(chunk.message_id.0, content::from_v1(chunk.content)?)
            .sender_session_id(
                chunk
                    .sender_session_id
                    .map(|id| acp_v2::SessionId::new(id.0)),
            )
            .recipient_session_id(
                chunk
                    .recipient_session_id
                    .map(|id| acp_v2::SessionId::new(id.0)),
            )
            .meta(chunk.meta),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentConnection as _, StubAgentConnection};
    use gpui::TestAppContext;
    use project::{FakeFs, Project};
    use serde_json::json;
    use settings::SettingsStore;
    use std::{path::Path, rc::Rc};
    use util::{path, path_list::PathList};

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let mut settings_store = SettingsStore::test(cx);
            settings_store.register_setting::<feature_flags::FeatureFlagsSettings>();
            cx.set_global(settings_store);
        });
    }

    async fn new_parent_thread(
        receipt_submissions: bool,
        cx: &mut TestAppContext,
    ) -> (Entity<AcpThread>, Rc<StubAgentConnection>) {
        init_test(cx);
        let project = Project::test(FakeFs::new(cx.executor()), [], cx).await;
        let connection =
            Rc::new(StubAgentConnection::new().with_receipt_submissions(receipt_submissions));
        let thread = cx
            .update(|cx| {
                connection.clone().new_session(
                    project,
                    PathList::new(&[Path::new(path!("/test"))]),
                    cx,
                )
            })
            .await
            .expect("stub session");
        (thread, connection)
    }

    fn spawn_child(
        parent: &Entity<AcpThread>,
        id: &str,
        cx: &mut TestAppContext,
    ) -> Entity<AcpThread> {
        parent.update(cx, |parent, cx| {
            parent.spawn_reported_subagent(acp_v2::SessionId::new(id), cx)
        })
    }

    fn apply(child: &Entity<AcpThread>, update: acp_v2::SubagentUpdate, cx: &mut TestAppContext) {
        child
            .update(cx, |child, cx| child.apply_subagent_update(update, cx))
            .expect("subagent update applies");
    }

    fn idle(stop_reason: acp_v2::StopReason) -> acp_v2::StateUpdate {
        acp_v2::StateUpdate::Idle(acp_v2::IdleStateUpdate::new().stop_reason(stop_reason))
    }

    fn running() -> acp_v2::StateUpdate {
        acp_v2::StateUpdate::Running(acp_v2::RunningStateUpdate::new())
    }

    #[gpui::test]
    async fn test_parent_owns_reported_subagents(cx: &mut TestAppContext) {
        let (parent, _connection) = new_parent_thread(false, cx).await;
        let spawned = Rc::new(std::cell::RefCell::new(Vec::new()));
        let _subscription = cx.update(|cx| {
            let spawned = spawned.clone();
            cx.subscribe(&parent, move |_, event: &AcpThreadEvent, _| {
                if let AcpThreadEvent::SubagentSpawned(id) = event {
                    spawned.borrow_mut().push(id.clone());
                }
            })
        });

        let child = spawn_child(&parent, "child", cx);
        let same_child = spawn_child(&parent, "child", cx);
        assert_eq!(child.entity_id(), same_child.entity_id());
        assert_eq!(*spawned.borrow(), vec![acp_v2::SessionId::new("child")]);
        child.read_with(cx, |child, cx| {
            assert_eq!(
                child.parent_session_id(),
                Some(parent.read(cx).session_id())
            );
            assert!(child.subagent_info().is_some());
            assert_eq!(child.run_status(), AgentRunStatus::Starting);
        });
        parent.read_with(cx, |parent, _| {
            assert!(
                parent
                    .subagent_thread(&acp_v2::SessionId::new("child"))
                    .is_some()
            );
        });

        let weak_child = child.downgrade();
        drop((child, same_child));
        cx.run_until_parked();
        assert!(
            weak_child.upgrade().is_some(),
            "the parent keeps announced children alive"
        );
        let weak_parent = parent.downgrade();
        drop(parent);
        cx.run_until_parked();
        assert!(weak_parent.upgrade().is_none());
        // The parent drops its handle while being released, so the child goes
        // in the following effect cycle.
        cx.update(|_| {});
        cx.run_until_parked();
        assert!(
            weak_child.upgrade().is_none(),
            "children are released with their parent"
        );
    }

    #[gpui::test]
    async fn test_subagent_update_patches_fields(cx: &mut TestAppContext) {
        let (parent, _connection) = new_parent_thread(false, cx).await;
        let child = spawn_child(&parent, "child", cx);

        apply(
            &child,
            acp_v2::SubagentUpdate::new("child")
                .title("Explore auth")
                .description("Find call sites")
                .capabilities(
                    acp_v2::SubagentSessionCapabilities::new()
                        .cancel(acp_v2::SessionCancelCapabilities::new()),
                )
                .state(running()),
            cx,
        );
        child.read_with(cx, |child, _| {
            let info = child.subagent_info().expect("subagent info");
            assert_eq!(info.title.as_deref(), Some("Explore auth"));
            assert_eq!(info.description.as_deref(), Some("Find call sites"));
            assert!(info.can_cancel);
            assert_eq!(child.title().as_deref(), Some("Explore auth"));
            assert_eq!(child.run_status(), AgentRunStatus::Running);
            assert!(
                child.foreground_activity().is_active(),
                "a child without its own state stream follows the parent's report"
            );
        });

        apply(
            &child,
            acp_v2::SubagentUpdate::new("child")
                .description(None::<String>)
                .capabilities(None::<acp_v2::SubagentSessionCapabilities>),
            cx,
        );
        child.read_with(cx, |child, _| {
            let info = child.subagent_info().expect("subagent info");
            assert_eq!(
                info.title.as_deref(),
                Some("Explore auth"),
                "omitted fields are kept"
            );
            assert_eq!(info.description, None, "null clears a field");
            assert!(!info.can_cancel);
            assert_eq!(child.run_status(), AgentRunStatus::Running);
        });

        for (state, expected) in [
            (
                acp_v2::StateUpdate::RequiresAction(acp_v2::RequiresActionStateUpdate::new()),
                AgentRunStatus::WaitingForUser,
            ),
            (idle(acp_v2::StopReason::EndTurn), AgentRunStatus::Done),
            (running(), AgentRunStatus::Running),
            (
                idle(acp_v2::StopReason::Cancelled),
                AgentRunStatus::Cancelled,
            ),
            (running(), AgentRunStatus::Running),
            (
                idle(acp_v2::StopReason::Error(acp_v2::ErrorStopReason::new())),
                AgentRunStatus::Failed,
            ),
            (
                acp_v2::StateUpdate::Unknown(acp_v2::UnknownStateUpdate::new()),
                AgentRunStatus::Unknown,
            ),
        ] {
            apply(
                &child,
                acp_v2::SubagentUpdate::new("child").state(state),
                cx,
            );
            child.read_with(cx, |child, _| assert_eq!(child.run_status(), expected));
        }

        let other = acp_v2::SubagentUpdate::new("other").title("Wrong session");
        assert!(
            child
                .update(cx, |child, cx| child.apply_subagent_update(other, cx))
                .is_err()
        );
    }

    #[gpui::test]
    async fn test_receipt_subagent_does_not_apply_parent_state_twice(cx: &mut TestAppContext) {
        let (parent, _connection) = new_parent_thread(true, cx).await;
        let child = spawn_child(&parent, "child", cx);

        apply(
            &child,
            acp_v2::SubagentUpdate::new("child").state(running()),
            cx,
        );
        child.read_with(cx, |child, _| {
            assert_eq!(child.activity_generation(), 0);
            assert_eq!(child.run_status(), AgentRunStatus::Running);
        });

        child
            .update(cx, |child, cx| {
                child.update_session_state(idle(acp_v2::StopReason::EndTurn), cx)
            })
            .expect("the child's own state applies");
        child.read_with(cx, |child, _| {
            assert_eq!(child.run_status(), AgentRunStatus::Done);
        });
    }

    #[gpui::test]
    async fn test_session_messages_become_task_entries(cx: &mut TestAppContext) {
        let (parent, _connection) = new_parent_thread(false, cx).await;
        let child = spawn_child(&parent, "child", cx);
        let parent_id = parent.read_with(cx, |parent, _| parent.session_id().clone());

        let message = acp_v2::SessionMessage::new("task-1")
            .sender_session_id(parent_id.clone())
            .content(vec![acp_v2::ContentBlock::from("Find the auth call sites")]);
        let inserted = child
            .update(cx, |child, cx| {
                child.upsert_session_message(message.clone(), cx)
            })
            .expect("message applies");
        assert_eq!(inserted, Some(0));
        let replayed = child
            .update(cx, |child, cx| child.upsert_session_message(message, cx))
            .expect("replayed message applies");
        assert_eq!(replayed, None, "replaying a message updates it in place");

        let outgoing = acp_v2::SessionMessage::new("reply")
            .sender_session_id(acp_v2::SessionId::new("child"))
            .content(vec![acp_v2::ContentBlock::from("Done")]);
        let ignored = child
            .update(cx, |child, cx| child.upsert_session_message(outgoing, cx))
            .expect("outgoing message is accepted");
        assert_eq!(ignored, None);

        let chunk =
            acp_v2::SessionMessageChunk::new("task-2", acp_v2::ContentBlock::from("Follow up"))
                .sender_session_id(parent_id.clone());
        let chunk_index = child
            .update(cx, |child, cx| {
                child.append_session_message_chunk(chunk, cx)
            })
            .expect("chunk applies");
        assert_eq!(chunk_index, Some(1));

        child.read_with(cx, |child, cx| {
            assert_eq!(child.entries().len(), 2);
            let AgentThreadEntry::UserMessage(task) = &child.entries()[0] else {
                panic!("expected the delegated task as a user entry");
            };
            assert_eq!(task.sender_session_id.as_ref(), Some(&parent_id));
            assert_eq!(task.content.to_markdown(cx), "Find the auth call sites");
            assert_eq!(child.session_message_range(0), 0..1);
            assert_eq!(child.session_message_range(1), 1..2);
        });
    }

    #[gpui::test]
    async fn test_client_subagent_link_survives_metadata_patches(cx: &mut TestAppContext) {
        let (parent, _connection) = new_parent_thread(false, cx).await;
        let link = |start| SubagentSessionInfo {
            session_id: acp_v2::SessionId::new("child"),
            message_start_index: start,
            message_end_index: None,
        };

        parent
            .update(cx, |parent, cx| {
                parent.upsert_wire_tool_call(
                    acp_v2::ToolCallUpdate::new("task-1").title("Explore auth"),
                    cx,
                )
            })
            .expect("tool call applies");
        parent.update(cx, |parent, cx| {
            parent.link_subagent_tool_call(acp_v2::ToolCallId::new("task-1"), link(0), cx);
            parent.link_subagent_tool_call(acp_v2::ToolCallId::new("task-2"), link(3), cx);
        });
        parent
            .update(cx, |parent, cx| {
                parent.upsert_wire_tool_call(
                    acp_v2::ToolCallUpdate::new("task-1").meta(acp_v2::Meta::from_iter([(
                        "claudeCode".into(),
                        json!({ "toolName": "Agent" }),
                    )])),
                    cx,
                )?;
                parent.upsert_wire_tool_call(
                    acp_v2::ToolCallUpdate::new("task-2").title("Write tests"),
                    cx,
                )
            })
            .expect("tool calls apply");

        parent.read_with(cx, |parent, _| {
            let (_, first) = parent
                .tool_call(&acp_v2::ToolCallId::new("task-1"))
                .expect("first tool call");
            let first = first.subagent_session_info.as_ref().expect("linked");
            assert_eq!(first.message_start_index, 0);
            let (_, second) = parent
                .tool_call(&acp_v2::ToolCallId::new("task-2"))
                .expect("second tool call");
            let second = second
                .subagent_session_info
                .as_ref()
                .expect("a pending link applies once the tool call arrives");
            assert_eq!(second.message_start_index, 3);
            assert!(
                parent
                    .tool_call_for_subagent(&acp_v2::SessionId::new("child"))
                    .is_some()
            );
        });
    }

    #[gpui::test]
    async fn test_cancelling_reported_subagent(cx: &mut TestAppContext) {
        let (parent, connection) = new_parent_thread(false, cx).await;
        let child = spawn_child(&parent, "child", cx);
        apply(
            &child,
            acp_v2::SubagentUpdate::new("child").state(running()),
            cx,
        );

        let refused = child.update(cx, |child, cx| child.cancel(cx));
        assert!(
            refused.await.is_err(),
            "cancelling needs the agent's permission"
        );
        assert!(connection.take_cancel_calls().is_empty());

        apply(
            &child,
            acp_v2::SubagentUpdate::new("child").capabilities(
                acp_v2::SubagentSessionCapabilities::new()
                    .cancel(acp_v2::SessionCancelCapabilities::new()),
            ),
            cx,
        );
        let cancelled = child.update(cx, |child, cx| child.cancel(cx));
        assert_eq!(
            connection.take_cancel_calls(),
            vec![acp_v2::SessionId::new("child")]
        );
        apply(
            &child,
            acp_v2::SubagentUpdate::new("child").state(idle(acp_v2::StopReason::Cancelled)),
            cx,
        );
        cancelled
            .await
            .expect("cancellation completes once the agent reports idle");
        child.read_with(cx, |child, _| {
            assert_eq!(child.run_status(), AgentRunStatus::Cancelled);
        });
    }

    #[test]
    fn test_v1_subagent_payloads_convert() {
        let update: acp_v1::SubagentUpdate = serde_json::from_value(json!({
            "sessionId": "child",
            "title": "Explore",
            "description": null,
            "capabilities": { "cancel": {} },
            "state": { "state": "requires_action" }
        }))
        .expect("v1 update");
        let update = subagent_update_from_v1(update).expect("converts");
        assert_eq!(update.session_id, acp_v2::SessionId::new("child"));
        assert!(matches!(update.title, MaybeUndefined::Value(ref title) if title == "Explore"));
        assert!(matches!(update.description, MaybeUndefined::Null));
        assert!(matches!(
            update.state,
            MaybeUndefined::Value(acp_v2::StateUpdate::RequiresAction(_))
        ));

        let message: acp_v1::SessionMessage = serde_json::from_value(json!({
            "messageId": "task-1",
            "senderSessionId": "parent",
            "content": [{ "type": "text", "text": "Go" }]
        }))
        .expect("v1 message");
        let message = session_message_from_v1(message).expect("converts");
        assert_eq!(
            message.sender_session_id,
            Some(acp_v2::SessionId::new("parent"))
        );
        assert!(matches!(message.content, MaybeUndefined::Value(ref blocks) if blocks.len() == 1));
    }
}
