use std::sync::{Arc, Mutex};

/// 服务端共享状态，用于 cwd 和 first-session 命令注入。
pub struct SessionState {
    /// 所有 shell 的工作目录。
    pub cwd: Option<String>,
    /// 第一个 shell 启动时自动注入的命令。
    pub initial_cmd: Option<String>,
    /// 是否已注入。
    first_session_done: Arc<Mutex<bool>>,
}

impl Clone for SessionState {
    fn clone(&self) -> Self {
        Self {
            cwd: self.cwd.clone(),
            initial_cmd: self.initial_cmd.clone(),
            first_session_done: Arc::clone(&self.first_session_done),
        }
    }
}

impl SessionState {
    pub fn new(cwd: Option<String>, initial_cmd: Option<String>) -> Self {
        Self {
            cwd,
            initial_cmd,
            first_session_done: Arc::new(Mutex::new(false)),
        }
    }

    /// 原子地尝试标记为已注入。返回 `true` 表示本调用者应执行注入。
    pub fn try_mark_done(&self) -> bool {
        let mut done = self.first_session_done.lock().unwrap();
        if *done {
            return false;
        }
        *done = true;
        true
    }
}
