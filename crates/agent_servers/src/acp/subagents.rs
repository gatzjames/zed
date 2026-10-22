use super::AcpSession;
use acp_thread::{AcpThread, SubagentSessionInfo};
use agent_client_protocol::schema::{v1 as acp_v1, v2 as acp_v2};
use anyhow::{Context as _, Result};
use collections::HashMap;
use gpui::{App, AsyncApp, Entity};
use std::{cell::RefCell, rc::Rc};

type Sessions = Rc<RefCell<HashMap<acp_v2::SessionId, AcpSession>>>;

/// Session updates that the connection, rather than the thread, must apply
/// because they create sessions or link one session's entries to another's.
pub(super) enum SubagentTraffic {
    Update(acp_v2::SubagentUpdate),
    Message(SessionMessageUpdate),
}

pub(super) enum SessionMessageUpdate {
    Message(acp_v2::SessionMessage),
    Chunk(acp_v2::SessionMessageChunk),
}

/// Converts v1 subagent traffic, or returns `None` for ordinary session traffic.
pub(super) fn subagent_traffic_from_v1(
    update: &acp_v1::SessionUpdate,
) -> Option<Result<SubagentTraffic>> {
    Some(match update {
        acp_v1::SessionUpdate::SubagentUpdate(update) => {
            acp_thread::subagent_update_from_v1(update.clone()).map(SubagentTraffic::Update)
        }
        acp_v1::SessionUpdate::SessionMessage(message) => {
            acp_thread::session_message_from_v1(message.clone())
                .map(|message| SubagentTraffic::Message(SessionMessageUpdate::Message(message)))
        }
        acp_v1::SessionUpdate::SessionMessageChunk(chunk) => {
            acp_thread::session_message_chunk_from_v1(chunk.clone())
                .map(|chunk| SubagentTraffic::Message(SessionMessageUpdate::Chunk(chunk)))
        }
        _ => return None,
    })
}

/// Applies subagent traffic received on `thread`'s session.
pub(super) fn apply_subagent_traffic(
    thread: &Entity<AcpThread>,
    traffic: SubagentTraffic,
    sessions: &Sessions,
    cx: &mut AsyncApp,
) -> Result<()> {
    match traffic {
        SubagentTraffic::Update(update) => apply_subagent_update(thread, update, sessions, cx),
        SubagentTraffic::Message(message) => apply_session_message(thread, message, sessions, cx),
    }
}

/// Applies a `subagent_update` received on `parent`'s session, creating and
/// registering the child before any of its own traffic can arrive.
fn apply_subagent_update(
    parent: &Entity<AcpThread>,
    update: acp_v2::SubagentUpdate,
    sessions: &Sessions,
    cx: &mut AsyncApp,
) -> Result<()> {
    let child_id = update.session_id.clone();
    let parent_id = parent.read_with(cx, |parent, _| parent.session_id().clone());
    let existing = sessions
        .borrow()
        .get(&child_id)
        .map(|session| (session.thread.upgrade(), session.subagent_parent.clone()));
    let child = match existing {
        Some((Some(child), owner)) => {
            // The protocol never transfers ownership of an announced child.
            anyhow::ensure!(
                owner.as_ref() == Some(&parent_id),
                "Session {child_id} is not a subagent of {parent_id}"
            );
            child
        }
        Some((None, _)) | None => {
            let child = parent.update(cx, |parent, cx| {
                parent.spawn_reported_subagent(child_id.clone(), cx)
            });
            cx.update(|cx| register_child_session(sessions, parent_id, &child, cx));
            child
        }
    };
    child.update(cx, |child, cx| child.apply_subagent_update(update, cx))
}

/// Applies a message that another session sent to `recipient`, and links the
/// sender's delegating tool call to the work that follows it.
fn apply_session_message(
    recipient: &Entity<AcpThread>,
    update: SessionMessageUpdate,
    sessions: &Sessions,
    cx: &mut AsyncApp,
) -> Result<()> {
    let (message_id, sender_id) = match &update {
        SessionMessageUpdate::Message(message) => (
            message.message_id.clone(),
            message.sender_session_id.clone(),
        ),
        SessionMessageUpdate::Chunk(chunk) => {
            (chunk.message_id.clone(), chunk.sender_session_id.clone())
        }
    };
    let inserted_index = recipient.update(cx, |recipient, cx| match update {
        SessionMessageUpdate::Message(message) => recipient.upsert_session_message(message, cx),
        SessionMessageUpdate::Chunk(chunk) => recipient.append_session_message_chunk(chunk, cx),
    })?;
    let Some(message_start_index) = inserted_index else {
        return Ok(());
    };
    let (recipient_id, parent_id, is_subagent) = recipient.read_with(cx, |recipient, _| {
        (
            recipient.session_id().clone(),
            recipient.parent_session_id().cloned(),
            recipient.subagent_info().is_some(),
        )
    });
    // Only delegations into a subagent turn the sending tool call into its card.
    if !is_subagent {
        return Ok(());
    }
    let Some(sender_id) = sender_id.or(parent_id) else {
        return Ok(());
    };
    let sender = sessions
        .borrow()
        .get(&sender_id)
        .and_then(|session| session.thread.upgrade())
        .with_context(|| format!("Unknown sender session {sender_id}"))?;
    sender.update(cx, |sender, cx| {
        sender.link_subagent_tool_call(
            acp_v2::ToolCallId::new(message_id.0),
            SubagentSessionInfo {
                session_id: recipient_id,
                message_start_index,
                message_end_index: None,
            },
            cx,
        );
    });
    Ok(())
}

fn register_child_session(
    sessions: &Sessions,
    parent_id: acp_v2::SessionId,
    child: &Entity<AcpThread>,
    cx: &mut App,
) {
    let session_id = child.read(cx).session_id().clone();
    let release_subscription = cx.observe_release(child, {
        let sessions = Rc::downgrade(sessions);
        let session_id = session_id.clone();
        let thread_id = child.entity_id();
        move |_, _| {
            let Some(sessions) = sessions.upgrade() else {
                return;
            };
            // Children are restricted sessions, so releasing one only forgets it.
            let Ok(mut sessions) = sessions.try_borrow_mut() else {
                log::debug!("Subagent session {session_id} was released during a session update");
                return;
            };
            if sessions
                .get(&session_id)
                .is_some_and(|session| session.thread.entity_id() == thread_id)
            {
                sessions.remove(&session_id);
            }
        }
    });
    sessions.borrow_mut().insert(
        session_id,
        AcpSession {
            thread: child.downgrade(),
            suppress_abort_err: false,
            session_modes: None,
            config_options: None,
            default_config_task: None,
            subagent_parent: Some(parent_id),
            _release_subscription: release_subscription,
        },
    );
}
