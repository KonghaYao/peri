use peri_middlewares::ask_user::{AskUserBatchRequest, AskUserQuestionData}; // P4b: type-dependency, oneshot channels

use crate::app::FieldTextarea;

// ─── AskUserBatchPrompt ───────────────────────────────────────────────────────

/// 单个问题的交互状态
pub struct QuestionState {
    pub data: AskUserQuestionData,
    pub option_cursor: isize, // 当前光标在第几个选项（最后一项 = 自定义输入行）
    pub selected: Vec<bool>,
    pub custom_input: FieldTextarea,
    pub in_custom_input: bool,
}

impl QuestionState {
    pub(super) fn new(data: AskUserQuestionData) -> Self {
        let len = data.options.len();
        Self {
            data,
            option_cursor: 0,
            selected: vec![false; len],
            custom_input: FieldTextarea::multi_line(5),
            in_custom_input: false,
        }
    }

    pub fn total_rows(&self) -> isize {
        self.data.options.len() as isize + 1
    }

    pub fn move_option_cursor(&mut self, delta: isize) {
        let total = self.total_rows();
        if total == 0 {
            return;
        }
        self.option_cursor = (self.option_cursor + delta).rem_euclid(total);
        self.in_custom_input = self.option_cursor == self.data.options.len() as isize;
    }

    pub fn toggle_current(&mut self) {
        if self.in_custom_input {
            return;
        }
        let i = self.option_cursor as usize;
        if i < self.selected.len() {
            if self.data.multi_select {
                self.selected[i] = !self.selected[i];
            } else {
                self.selected.iter_mut().for_each(|v| *v = false);
                self.selected[i] = true;
            }
        }
    }

    /// 收集当前问题的答案文本
    pub fn answer(&self) -> String {
        let mut parts: Vec<String> = self
            .selected
            .iter()
            .enumerate()
            .filter(|(_, &v)| v)
            .map(|(i, _)| self.data.options[i].label.clone())
            .collect();
        let custom = self.custom_input.value().trim().to_string();
        if !custom.is_empty() {
            parts.push(custom);
        }
        if parts.is_empty() {
            self.custom_input.value().trim().to_string()
        } else {
            parts.join(", ")
        }
    }
}

/// 批量 AskUser 弹窗：多个问题用 Tab 切换，Enter 逐题确认，全部确认后提交
pub struct AskUserBatchPrompt {
    pub questions: Vec<QuestionState>,
    /// 当前激活的问题 tab 索引
    pub active_tab: usize,
    /// 每个问题是否已按 Enter 确认
    pub confirmed: Vec<bool>,
    pub response_tx: tokio::sync::oneshot::Sender<Vec<String>>,
    /// 内容滚动偏移
    pub scroll_offset: u16,
    /// 渲染时存储的滚动条几何信息���供事件交互使用
    /// 渲染时存储的滚动条几何信息，供事件交互使用
    pub scrollbar_metrics: Option<peri_widgets::ScrollbarMetrics>,
    /// 渲染时构建的选项→行号映射，供滚动定位使用
    /// option_row_map[i] = 选项 i 在渲染内���中的起始行号
    pub option_row_map: Vec<u16>,
}

impl AskUserBatchPrompt {
    pub fn from_request(req: AskUserBatchRequest) -> Self {
        let len = req.questions.len();
        let questions = req.questions.into_iter().map(QuestionState::new).collect();
        Self {
            questions,
            active_tab: 0,
            confirmed: vec![false; len],
            response_tx: req.response_tx,
            scroll_offset: 0,
            scrollbar_metrics: None,
            option_row_map: Vec::new(),
        }
    }

    pub fn next_tab(&mut self) {
        if !self.questions.is_empty() {
            self.active_tab = (self.active_tab + 1) % self.questions.len();
        }
    }

    pub fn prev_tab(&mut self) {
        if !self.questions.is_empty() {
            self.active_tab = self
                .active_tab
                .checked_sub(1)
                .unwrap_or(self.questions.len() - 1);
        }
    }

    pub fn current(&mut self) -> &mut QuestionState {
        &mut self.questions[self.active_tab]
    }

    /// Enter 确认当前问题：标记已确认，跳到下一未确认的问题。
    /// 若所有问题都已确认，返回 true（调用方负责调用 confirm()）。
    pub fn confirm_current(&mut self) -> bool {
        self.confirmed[self.active_tab] = true;

        if self.confirmed.iter().all(|&c| c) {
            return true;
        }

        // 跳到下一个未确认的问题
        let n = self.questions.len();
        for offset in 1..=n {
            let next = (self.active_tab + offset) % n;
            if !self.confirmed[next] {
                self.active_tab = next;
                break;
            }
        }
        false
    }

    pub fn confirm(self) {
        let answers: Vec<String> = self.questions.iter().map(|q| q.answer()).collect();
        let _ = self.response_tx.send(answers);
    }
}
