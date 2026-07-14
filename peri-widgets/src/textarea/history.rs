use crate::textarea::state::TextAreaState;

/// 编辑快照（用于 undo/redo）
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub text: String,
    pub cursor: usize,
}

/// 一条编辑记录：包含编辑前后的状态快照
#[derive(Debug, Clone)]
struct EditRecord {
    before: Snapshot,
    after: Snapshot,
}

/// 撤销/重做双向栈
#[derive(Debug, Clone)]
pub struct History {
    undo_stack: Vec<EditRecord>,
    redo_stack: Vec<EditRecord>,
    max_depth: usize,
}

impl Default for History {
    fn default() -> Self {
        Self {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            max_depth: 100,
        }
    }
}

impl History {
    /// 记录一次编辑操作。
    /// `before`: 操作前的状态快照，`after`: 操作后的状态快照（当前 state）。
    pub fn record(&mut self, before: Snapshot, after: Snapshot) {
        let record = EditRecord { before, after };
        self.undo_stack.push(record);
        while self.undo_stack.len() > self.max_depth {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear();
    }

    /// 撤销：用 before 状态恢复 state 的各个字段，当前状态压入 redo 栈。
    /// 接收各字段的独立可变引用以避免借用冲突。
    pub fn undo(
        &mut self,
        text: &mut String,
        cursor: &mut usize,
        selection_start: &mut Option<usize>,
    ) -> bool {
        if let Some(record) = self.undo_stack.pop() {
            // 保存当前状态到 redo 栈
            self.redo_stack.push(EditRecord {
                before: record.after.clone(),
                after: Snapshot {
                    text: text.clone(),
                    cursor: *cursor,
                },
            });
            // 恢复 before 状态
            *text = record.before.text;
            *cursor = record.before.cursor;
            *selection_start = None;
            true
        } else {
            false
        }
    }

    /// 重做：用 after 状态恢复 state 的各个字段。
    pub fn redo(
        &mut self,
        text: &mut String,
        cursor: &mut usize,
        selection_start: &mut Option<usize>,
    ) -> bool {
        if let Some(record) = self.redo_stack.pop() {
            // 保存当前状态到 undo 栈
            let before = Snapshot {
                text: text.clone(),
                cursor: *cursor,
            };
            self.undo_stack.push(EditRecord {
                before,
                after: record.before.clone(),
            });
            // 恢复 after 状态
            *text = record.after.text;
            *cursor = record.after.cursor;
            *selection_start = None;
            true
        } else {
            false
        }
    }

    /// 提交后清空 undo/redo 栈。
    pub fn clear(&mut self) {
        self.undo_stack.clear();
        self.redo_stack.clear();
    }

    pub fn snapshot(state: &TextAreaState) -> Snapshot {
        Snapshot {
            text: state.text.clone(),
            cursor: state.cursor,
        }
    }
}
