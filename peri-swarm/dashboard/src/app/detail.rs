//! Session-detail scheduling, mutations and worker results.
//!
//! The detail page refreshes as one group of three requests: the snapshot, the
//! newest timeline page and the newest turns page. One group is on the wire at
//! a time; a group that started under an older generation is discarded. A
//! failed refresh keeps the last good snapshot and marks it stale.

use std::time::Instant;

use crate::api::{
    ApiError, ApprovalDecision, ApprovalReceipt, IdempotencyKey, ItemsPage, Session, Turns,
};
use crate::poll;
use crate::ui::text;
use crate::worker::{MutationData, MutationKind};

use super::session_detail::{
    ApprovalState, SendState, SessionDetailState, TIMELINE_PAGE_SIZE, TURNS_PAGE_SIZE,
};
use super::types::{ActionId, DetailTab, Effect, FieldId, Focus, Intent, Overlay, Screen};
use super::App;

impl App {
    // -----------------------------------------------------------------------
    // Scheduling
    // -----------------------------------------------------------------------

    /// Detail scheduling. Returns the group of effects when a refresh is due.
    pub fn detail_due(&mut self, now: Instant) -> Vec<Effect> {
        if self.config.is_none()
            || self.screen != Screen::SessionDetail
            || self.overlay.is_some()
            || self.detail.id.is_none()
        {
            return Vec::new();
        }
        if self.paused {
            self.detail.poll.pause();
            return Vec::new();
        }
        self.detail.poll.resume();
        // A pause/resume bumps the generation; release page markers that can
        // no longer receive an applied response before the early returns.
        self.detail.reconcile_pagination();
        if self.detail.outstanding > 0 {
            return Vec::new();
        }
        if self.detail.timeline.loading_more || self.detail.turns.loading_more {
            return Vec::new();
        }
        let interval = self
            .detail
            .poll
            .delay(poll::detail_interval(self.detail.status()));
        let due = self.detail.reload_wanted
            || self
                .detail
                .last_request
                .is_none_or(|last| now.saturating_duration_since(last) >= interval);
        if !due {
            return Vec::new();
        }
        self.start_detail_group(now)
    }

    /// Refetch the snapshot and newest pages now (`r` and after mutations).
    pub(super) fn refresh_detail_now(&mut self) -> Vec<Effect> {
        if self.screen != Screen::SessionDetail || self.detail.id.is_none() {
            return Vec::new();
        }
        self.detail.poll.reset_backoff();
        // A manual refresh invalidates responses from the previous group,
        // including an in-flight `Load earlier` / `Load more` page. Release
        // those markers now: their responses will be discarded and leaving
        // them set would keep `detail_due` from ever starting another group.
        self.detail.poll.invalidate();
        self.detail.cancel_pagination();
        self.detail.reload_wanted = true;
        self.detail_due(Instant::now())
    }

    fn start_detail_group(&mut self, now: Instant) -> Vec<Effect> {
        let Some(session_id) = self.detail.id.clone() else {
            return Vec::new();
        };
        let Some(generation) = self.detail.poll.begin() else {
            return Vec::new();
        };
        self.detail.group_generation = generation;
        self.detail.outstanding = 3;
        self.detail.group_failed = false;
        self.detail.reload_wanted = false;
        self.detail.last_request = Some(now);
        self.detail.loading = self.detail.session.is_none();
        let epoch = self.epoch;
        vec![
            Effect::GetSession {
                epoch,
                generation,
                session_id: session_id.clone(),
            },
            Effect::ListItems {
                epoch,
                generation,
                session_id: session_id.clone(),
                limit: TIMELINE_PAGE_SIZE,
                after: None,
            },
            Effect::ListTurns {
                epoch,
                generation,
                session_id,
                limit: TURNS_PAGE_SIZE,
                after: None,
            },
        ]
    }

    /// Release one slot of the in-flight group and record the group outcome.
    fn finish_detail_response(&mut self, generation: u64, success: bool) {
        if self.detail.outstanding == 0 {
            return;
        }
        self.detail.outstanding -= 1;
        if !success {
            self.detail.group_failed = true;
        }
        if self.detail.outstanding == 0 {
            if self.detail.group_failed {
                self.detail.poll.fail(generation);
            } else {
                self.detail.poll.complete(generation);
            }
        }
    }

    /// Whether a response still belongs to the current detail page and group.
    fn detail_response_current(&self, epoch: u64, generation: u64, session_id: &str) -> bool {
        epoch == self.epoch
            && generation == self.detail.group_generation
            && self.detail.id.as_deref() == Some(session_id)
    }

    // -----------------------------------------------------------------------
    // Worker results
    // -----------------------------------------------------------------------

    pub(super) fn handle_session_loaded(
        &mut self,
        epoch: u64,
        generation: u64,
        session_id: String,
        result: Result<Box<Session>, ApiError>,
    ) -> Vec<Effect> {
        if epoch != self.epoch {
            return Vec::new();
        }
        let current = self.detail_response_current(epoch, generation, &session_id);
        if !current {
            if self.detail.outstanding > 0 {
                self.finish_detail_response(generation, false);
                self.detail.reload_wanted = true;
            }
            return Vec::new();
        }
        let success = result.is_ok();
        match result {
            Ok(session) => {
                let ids: Vec<String> = session
                    .required_actions
                    .iter()
                    .filter_map(|action| {
                        action
                            .get("request_id")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned)
                    })
                    .collect();
                self.detail.answered.retain(|id| ids.contains(id));
                self.detail.session = Some(*session);
                self.detail.loading = false;
                self.detail.stale = false;
                self.detail.error = None;
            }
            Err(error) => self.apply_detail_read_error(&error, true),
        }
        self.finish_detail_response(generation, success);
        Vec::new()
    }

    pub(super) fn handle_items_loaded(
        &mut self,
        epoch: u64,
        generation: u64,
        session_id: String,
        after: Option<String>,
        result: Result<Box<ItemsPage>, ApiError>,
    ) -> Vec<Effect> {
        if epoch != self.epoch || self.detail.id.as_deref() != Some(session_id.as_str()) {
            return Vec::new();
        }
        if after.is_some() {
            // `Load earlier` is tracked with its own flag and cursor.
            if generation != self.detail.poll.generation() {
                self.detail.reconcile_pagination();
                return Vec::new();
            }
            match result {
                Ok(page) => self.detail.timeline.apply_older_page(&page),
                Err(error) => {
                    if error.is_unauthorized() {
                        self.apply_detail_unauthorized(&error);
                    }
                    let detail = text::api_error(&error);
                    self.detail
                        .timeline
                        .fail_older(text::timeline_error(&detail));
                }
            }
            return Vec::new();
        }
        let current = self.detail_response_current(epoch, generation, &session_id);
        if !current {
            if self.detail.outstanding > 0 {
                self.finish_detail_response(generation, false);
                self.detail.reload_wanted = true;
            }
            return Vec::new();
        }
        let success = result.is_ok();
        match result {
            Ok(page) => self.detail.timeline.apply_first_page(&page),
            Err(error) => {
                if error.is_unauthorized() {
                    self.apply_detail_unauthorized(&error);
                }
                let detail = text::api_error(&error);
                self.detail.timeline.error = Some(text::timeline_error(&detail));
                if self.detail.session.is_some() {
                    self.detail.stale = true;
                }
            }
        }
        self.finish_detail_response(generation, success);
        Vec::new()
    }

    pub(super) fn handle_turns_loaded(
        &mut self,
        epoch: u64,
        generation: u64,
        session_id: String,
        after: Option<String>,
        result: Result<Box<Turns>, ApiError>,
    ) -> Vec<Effect> {
        if epoch != self.epoch || self.detail.id.as_deref() != Some(session_id.as_str()) {
            return Vec::new();
        }
        if after.is_some() {
            if generation != self.detail.poll.generation() {
                self.detail.reconcile_pagination();
                return Vec::new();
            }
            match result {
                Ok(page) => self.detail.turns.apply_more(&page),
                Err(error) => {
                    if error.is_unauthorized() {
                        self.apply_detail_unauthorized(&error);
                    }
                    let detail = text::api_error(&error);
                    self.detail.turns.fail(text::turns_error(&detail));
                }
            }
            return Vec::new();
        }
        let current = self.detail_response_current(epoch, generation, &session_id);
        if !current {
            if self.detail.outstanding > 0 {
                self.finish_detail_response(generation, false);
                self.detail.reload_wanted = true;
            }
            return Vec::new();
        }
        let success = result.is_ok();
        match result {
            Ok(page) => self.detail.turns.apply_first_page(&page),
            Err(error) => {
                if error.is_unauthorized() {
                    self.apply_detail_unauthorized(&error);
                }
                let detail = text::api_error(&error);
                self.detail.turns.fail(text::turns_error(&detail));
            }
        }
        self.finish_detail_response(generation, success);
        Vec::new()
    }

    pub(super) fn handle_mutation(
        &mut self,
        epoch: u64,
        session_id: String,
        kind: MutationKind,
        result: Result<MutationData, ApiError>,
    ) -> Vec<Effect> {
        if epoch != self.epoch {
            return Vec::new();
        }
        match kind {
            MutationKind::Update => self.handle_update_finished(result),
            MutationKind::Delete => self.handle_delete_finished(session_id, result),
            MutationKind::SendInput => self.handle_send_finished(&session_id, result),
            MutationKind::CancelTurn => self.handle_cancel_finished(&session_id, result),
            MutationKind::Approval => self.handle_approval_finished(&session_id, result),
        }
    }

    fn handle_update_finished(&mut self, result: Result<MutationData, ApiError>) -> Vec<Effect> {
        if self.screen != Screen::SessionEdit {
            return Vec::new();
        }
        match result {
            Ok(MutationData::Updated(session)) => {
                self.edit.finish(true);
                self.detail.session = Some(*session);
                self.detail.loading = false;
                self.detail.stale = false;
                self.detail.error = None;
                self.detail.action_error = None;
                self.screen = Screen::SessionDetail;
                self.focus = Focus::Content;
                self.content_scroll = 0;
                self.detail.reload_wanted = true;
                Vec::new()
            }
            Ok(_) => {
                self.edit.finish(false);
                self.edit.form_error = Some(text::update_failed(text::DASH));
                Vec::new()
            }
            Err(error) => {
                self.edit.finish(false);
                let already_unauthorized = error.is_unauthorized();
                let detail = text::api_error(&error);
                if error.is_conflict() {
                    self.edit.form_error = Some(text::update_conflict(&detail));
                    self.edit.offer_cancel = true;
                } else if error.is_bad_gateway() {
                    self.edit.form_error = Some(text::gateway_host_failure(&detail));
                } else {
                    self.edit.form_error = Some(text::update_failed(&detail));
                }
                if already_unauthorized {
                    self.apply_detail_unauthorized(&error);
                }
                Vec::new()
            }
        }
    }

    fn handle_delete_finished(
        &mut self,
        session_id: String,
        result: Result<MutationData, ApiError>,
    ) -> Vec<Effect> {
        if self.detail.id.as_deref() != Some(session_id.as_str()) {
            return Vec::new();
        }
        match result {
            Ok(MutationData::Deleted) => {
                self.detail = SessionDetailState::default();
                self.detail_tab = DetailTab::Timeline;
                self.screen = Screen::Main;
                self.page = super::types::Page::SessionLog;
                self.focus = Focus::Content;
                self.content_scroll = 0;
                self.session_log.reload_wanted = true;
                self.refresh_sessions_now()
            }
            Ok(_) => {
                self.detail.action_error = Some(super::session_detail::ActionError {
                    message: text::delete_failed(text::DASH),
                    offer_cancel: false,
                });
                Vec::new()
            }
            Err(error) => {
                if error.is_unauthorized() {
                    self.apply_detail_unauthorized(&error);
                }
                let detail = text::api_error(&error);
                let (message, offer_cancel) = if error.is_conflict() {
                    (text::delete_conflict(&detail), true)
                } else if error.is_bad_gateway() {
                    (text::gateway_host_failure(&detail), false)
                } else {
                    (text::delete_failed(&detail), false)
                };
                self.detail.action_error = Some(super::session_detail::ActionError {
                    message,
                    offer_cancel,
                });
                Vec::new()
            }
        }
    }

    fn handle_send_finished(
        &mut self,
        session_id: &str,
        result: Result<MutationData, ApiError>,
    ) -> Vec<Effect> {
        if self.detail.id.as_deref() != Some(session_id) {
            return Vec::new();
        }
        match result {
            Ok(MutationData::Input(receipt)) => {
                if receipt.accepted {
                    self.detail.send = None;
                    self.detail.composer.text.clear();
                    self.detail.composer.cursor = 0;
                    self.detail.composer_error = None;
                    self.detail.reload_wanted = true;
                } else {
                    self.mark_send_failed(text::send_failed(&receipt.disposition));
                }
                Vec::new()
            }
            Ok(_) => {
                self.mark_send_failed(text::send_failed(text::DASH));
                Vec::new()
            }
            Err(error) => {
                if error.is_unauthorized() {
                    self.apply_detail_unauthorized(&error);
                }
                let detail = text::api_error(&error);
                self.mark_send_failed(text::send_failed(&detail));
                Vec::new()
            }
        }
    }

    fn mark_send_failed(&mut self, message: String) {
        if let Some(send) = &mut self.detail.send {
            send.in_flight = false;
        }
        self.detail.composer_error = Some(message);
    }

    fn handle_cancel_finished(
        &mut self,
        session_id: &str,
        result: Result<MutationData, ApiError>,
    ) -> Vec<Effect> {
        if self.detail.id.as_deref() != Some(session_id) {
            return Vec::new();
        }
        match result {
            Ok(MutationData::Input(_)) => {
                self.detail.action_error = None;
                self.detail.reload_wanted = true;
            }
            Ok(_) => {
                self.detail.action_error = Some(super::session_detail::ActionError {
                    message: text::cancel_failed(text::DASH),
                    offer_cancel: false,
                });
            }
            Err(error) => {
                if error.is_unauthorized() {
                    self.apply_detail_unauthorized(&error);
                }
                let detail = text::api_error(&error);
                self.detail.action_error = Some(super::session_detail::ActionError {
                    message: text::cancel_failed(&detail),
                    offer_cancel: false,
                });
            }
        }
        Vec::new()
    }

    fn handle_approval_finished(
        &mut self,
        session_id: &str,
        result: Result<MutationData, ApiError>,
    ) -> Vec<Effect> {
        if self.detail.id.as_deref() != Some(session_id) {
            return Vec::new();
        }
        match result {
            Ok(MutationData::Approval(receipt)) => {
                self.apply_approval_receipt(&receipt);
                Vec::new()
            }
            Ok(_) => {
                self.detail.approval = None;
                self.detail.approval_error = Some(text::approval_failed(text::DASH));
                Vec::new()
            }
            Err(error) => {
                if error.is_unauthorized() {
                    self.apply_detail_unauthorized(&error);
                }
                let detail = text::api_error(&error);
                self.detail.approval = None;
                self.detail.approval_error = Some(text::approval_failed(&detail));
                self.detail.reload_wanted = true;
                Vec::new()
            }
        }
    }

    fn apply_approval_receipt(&mut self, receipt: &ApprovalReceipt) {
        self.detail.approval = None;
        self.detail.approval_error = None;
        self.detail.answered.insert(receipt.request_id.clone());
        self.detail.reload_wanted = true;
    }

    fn apply_detail_read_error(&mut self, error: &ApiError, snapshot: bool) {
        if error.is_unauthorized() {
            self.apply_detail_unauthorized(error);
        }
        let detail = text::api_error(error);
        if snapshot {
            if self.detail.session.is_some() {
                self.detail.stale = true;
            }
            self.detail.error = Some(text::detail_error(&detail));
        }
        self.detail.loading = false;
    }

    fn apply_detail_unauthorized(&mut self, error: &ApiError) {
        self.apply_unauthorized(error);
        self.detail.poll.pause();
    }

    // -----------------------------------------------------------------------
    // Actions
    // -----------------------------------------------------------------------

    pub(super) fn toggle_timeline_block(&mut self, id: &str) {
        if self.screen == Screen::SessionDetail {
            self.detail.timeline.toggle_block(id);
        }
    }

    pub(super) fn load_earlier_items(&mut self) -> Vec<Effect> {
        if self.screen != Screen::SessionDetail || self.overlay.is_some() {
            return Vec::new();
        }
        // A group refresh already carries a newest `ListItems`; a page request
        // must not overlap it.
        if self.detail.outstanding > 0 {
            return Vec::new();
        }
        let Some(session_id) = self.detail.id.clone() else {
            return Vec::new();
        };
        let generation = self.detail.poll.generation();
        let Some(after) = self.detail.timeline.begin_older(generation) else {
            return Vec::new();
        };
        vec![Effect::ListItems {
            epoch: self.epoch,
            generation,
            session_id,
            limit: TIMELINE_PAGE_SIZE,
            after: Some(after),
        }]
    }

    pub(super) fn load_more_turns(&mut self) -> Vec<Effect> {
        if self.screen != Screen::SessionDetail || self.overlay.is_some() {
            return Vec::new();
        }
        if self.detail.outstanding > 0 {
            return Vec::new();
        }
        let Some(session_id) = self.detail.id.clone() else {
            return Vec::new();
        };
        let generation = self.detail.poll.generation();
        let Some(after) = self.detail.turns.begin_more(generation) else {
            return Vec::new();
        };
        vec![Effect::ListTurns {
            epoch: self.epoch,
            generation,
            session_id,
            limit: TURNS_PAGE_SIZE,
            after: Some(after),
        }]
    }

    pub(super) fn jump_to_latest(&mut self) {
        self.detail.timeline.jump_to_latest();
        self.content_scroll = self.content_max;
    }

    pub(super) fn send_message(&mut self) -> Vec<Effect> {
        if self.screen != Screen::SessionDetail || self.overlay.is_some() {
            return Vec::new();
        }
        if self.detail.composer_disabled() || self.detail.send_in_flight() {
            return Vec::new();
        }
        let Some(session_id) = self.detail.id.clone() else {
            return Vec::new();
        };
        let message = self.detail.composer.text.clone();
        if message.trim().is_empty() {
            self.detail.composer_error = Some(text::ERR_MESSAGE_EMPTY.to_owned());
            return Vec::new();
        }
        let key = match &self.detail.send {
            Some(send) if send.text == message => send.key.clone(),
            _ => IdempotencyKey::new(),
        };
        self.detail.composer_error = None;
        self.detail.send = Some(SendState {
            text: message.clone(),
            key: key.clone(),
            in_flight: true,
        });
        vec![Effect::SendInput {
            epoch: self.epoch,
            session_id,
            text: message,
            key,
        }]
    }

    pub(super) fn answer_approval(
        &mut self,
        request_id: &str,
        decision: ApprovalDecision,
    ) -> Vec<Effect> {
        if self.screen != Screen::SessionDetail || self.detail.approval_in_flight() {
            return Vec::new();
        }
        let Some(session_id) = self.detail.id.clone() else {
            return Vec::new();
        };
        if !self
            .detail
            .approvals()
            .iter()
            .any(|approval| approval.request_id == request_id)
        {
            return Vec::new();
        }
        let key = match &self.detail.approval {
            Some(state) if state.request_id == request_id && state.decision == decision => {
                state.key.clone()
            }
            _ => IdempotencyKey::new(),
        };
        self.detail.approval_error = None;
        self.detail.approval = Some(ApprovalState {
            request_id: request_id.to_owned(),
            decision,
            key: key.clone(),
        });
        vec![Effect::AnswerApproval {
            epoch: self.epoch,
            session_id,
            request_id: request_id.to_owned(),
            decision,
            key,
        }]
    }

    pub(super) fn first_approval(&self) -> Option<String> {
        self.detail.first_approval_id()
    }

    pub(super) fn open_edit_session(&mut self) {
        if self.screen != Screen::SessionDetail {
            return;
        }
        let Some(session) = self.detail.session.clone() else {
            return;
        };
        self.edit = super::session_edit::EditFormState::from_session(&session);
        self.detail.action_error = None;
        self.screen = Screen::SessionEdit;
        self.focus = Focus::Field(FieldId::EditModel);
        self.content_scroll = 0;
    }

    pub(super) fn submit_edit(&mut self) -> Vec<Effect> {
        if self.screen != Screen::SessionEdit || self.edit.submitting() {
            return Vec::new();
        }
        let Some(session_id) = self.detail.id.clone() else {
            return Vec::new();
        };
        let Some((spec, key)) = self.edit.prepare_submission() else {
            return Vec::new();
        };
        vec![Effect::UpdateSession {
            epoch: self.epoch,
            session_id,
            spec,
            key,
        }]
    }

    pub(super) fn open_delete_session(&mut self) {
        if self.screen != Screen::SessionDetail || self.detail.id.is_none() {
            return;
        }
        self.detail.action_error = None;
        self.overlay = Some(Overlay::ConfirmDeleteSession);
        self.overlay_cursor = 0;
    }

    pub(super) fn confirm_delete_session(&mut self) -> Vec<Effect> {
        if self.overlay != Some(Overlay::ConfirmDeleteSession) {
            return Vec::new();
        }
        let Some(session_id) = self.detail.id.clone() else {
            return Vec::new();
        };
        self.overlay = None;
        vec![Effect::DeleteSession {
            epoch: self.epoch,
            session_id,
            key: IdempotencyKey::new(),
        }]
    }

    pub(super) fn open_cancel_turn(&mut self) {
        if self.screen != Screen::SessionDetail || !self.detail.is_running() {
            return;
        }
        self.detail.action_error = None;
        self.overlay = Some(Overlay::ConfirmCancelTurn);
        self.overlay_cursor = 0;
    }

    pub(super) fn confirm_cancel_turn(&mut self) -> Vec<Effect> {
        if self.overlay != Some(Overlay::ConfirmCancelTurn) {
            return Vec::new();
        }
        let Some(session_id) = self.detail.id.clone() else {
            return Vec::new();
        };
        self.overlay = None;
        vec![Effect::CancelTurn {
            epoch: self.epoch,
            session_id,
            key: IdempotencyKey::new(),
        }]
    }

    /// Route an action that only the detail/edit pages use.
    pub(super) fn detail_action(&mut self, action: ActionId) -> Option<Vec<Effect>> {
        let effects = match action {
            ActionId::RefreshDetail => self.refresh_detail_now(),
            ActionId::LoadEarlierItems => self.load_earlier_items(),
            ActionId::JumpToLatest => {
                self.jump_to_latest();
                Vec::new()
            }
            ActionId::LoadMoreTurns => self.load_more_turns(),
            ActionId::SendMessage => self.send_message(),
            ActionId::ApproveFirst => match self.first_approval() {
                Some(request_id) => self.answer_approval(&request_id, ApprovalDecision::Approve),
                None => Vec::new(),
            },
            ActionId::DenyFirst => match self.first_approval() {
                Some(request_id) => self.answer_approval(&request_id, ApprovalDecision::Deny),
                None => Vec::new(),
            },
            ActionId::EditSession => {
                self.open_edit_session();
                Vec::new()
            }
            ActionId::DeleteSession => {
                self.open_delete_session();
                Vec::new()
            }
            ActionId::CancelTurn => {
                self.open_cancel_turn();
                Vec::new()
            }
            ActionId::ConfirmDeleteSession => self.confirm_delete_session(),
            ActionId::CancelDeleteSession => {
                if self.overlay == Some(Overlay::ConfirmDeleteSession) {
                    self.overlay = None;
                }
                Vec::new()
            }
            ActionId::ConfirmCancelTurn => self.confirm_cancel_turn(),
            ActionId::CancelCancelTurn => {
                if self.overlay == Some(Overlay::ConfirmCancelTurn) {
                    self.overlay = None;
                }
                Vec::new()
            }
            ActionId::EditSubmit => self.submit_edit(),
            _ => return None,
        };
        Some(effects)
    }

    /// Route an intent that only the detail pages use.
    pub(super) fn detail_intent(&mut self, intent: Intent) -> Option<Vec<Effect>> {
        match intent {
            Intent::ToggleTimelineBlock(id) => {
                self.toggle_timeline_block(&id);
                Some(Vec::new())
            }
            Intent::LoadEarlierItems => Some(self.load_earlier_items()),
            Intent::JumpToLatest => {
                self.jump_to_latest();
                Some(Vec::new())
            }
            Intent::LoadMoreTurns => Some(self.load_more_turns()),
            Intent::RefreshDetail => Some(self.refresh_detail_now()),
            Intent::SendMessage => Some(self.send_message()),
            Intent::AnswerApproval {
                request_id,
                decision,
            } => Some(self.answer_approval(&request_id, decision)),
            Intent::OpenEditSession => {
                self.open_edit_session();
                Some(Vec::new())
            }
            Intent::SubmitEdit => Some(self.submit_edit()),
            Intent::OpenDeleteSession => {
                self.open_delete_session();
                Some(Vec::new())
            }
            Intent::OpenCancelTurn => {
                self.open_cancel_turn();
                Some(Vec::new())
            }
            other => match other {
                Intent::ConfirmDeleteSession => Some(self.confirm_delete_session()),
                Intent::CancelDeleteSession => {
                    if self.overlay == Some(Overlay::ConfirmDeleteSession) {
                        self.overlay = None;
                    }
                    Some(Vec::new())
                }
                Intent::ConfirmCancelTurn => Some(self.confirm_cancel_turn()),
                Intent::CancelCancelTurn => {
                    if self.overlay == Some(Overlay::ConfirmCancelTurn) {
                        self.overlay = None;
                    }
                    Some(Vec::new())
                }
                _ => None,
            },
        }
    }
}

#[cfg(test)]
#[path = "detail_test.rs"]
mod tests;
