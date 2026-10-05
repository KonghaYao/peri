"""RCRA 架构反例探针：有限抽象模型，不是生产实现或完整模型检查器。

运行：python3 scripts/rcra_message_contract_probe.py
每组演示弱契约的反例及补充守卫；仅两个场景枚举事件顺序。
其余为构造示例，不证明任意交错安全；原子步骤和持久存储均为假设。
不验证存储原子性、真实并发、SDK、MCP 或模型指令隔离。
"""

import itertools
import unittest


class ContractCounterexamples(unittest.TestCase):
    def test_logical_delivery_identity(self):
        events = [("owner", "event", "child", "result", "d1"),
                  ("owner", "event", "child", "result", "d2")]
        self.assertEqual(len({e[-1] for e in events}), 2)
        self.assertEqual(len({e[:-1] for e in events}), 1)

    def test_dispatch_requires_committed_response(self):
        schedules = list(itertools.permutations(("commit", "dispatch", "crash")))
        weak_unsafe = guarded_unsafe = 0
        for steps in schedules:
            for guarded in (False, True):
                committed = False
                unsafe = False
                for step in steps:
                    if step == "crash":
                        break
                    if step == "commit":
                        committed = True
                    if step == "dispatch" and (committed or not guarded):
                        unsafe |= not committed
                if guarded:
                    guarded_unsafe += unsafe
                else:
                    weak_unsafe += unsafe
        self.assertGreater(weak_unsafe, 0)
        self.assertEqual(guarded_unsafe, 0)

    def test_activation_pause_handoff(self):
        # start 是抽象原子迁移；真实准入交接原子性必须另行验证。
        def simulate(steps, check_pause=True, check_revision=True):
            revision, paused, ticket, started = 0, False, None, False
            violation = False
            for step in steps:
                if step == "admit":
                    ticket = None if paused else revision
                elif step == "pause":
                    revision += 1
                    paused = True
                elif step == "resume":
                    revision += 1
                    paused = False
                elif step == "start" and ticket is not None:
                    allowed = ((not check_pause or not paused)
                               and (not check_revision or ticket == revision))
                    if allowed:
                        started = True
                        violation |= paused or ticket != revision
            return started, violation

        schedules = [p for p in itertools.permutations(("admit", "pause", "resume", "start"))
                     if p.index("admit") < p.index("start")
                     and p.index("pause") < p.index("resume")]
        self.assertTrue(any(simulate(p, False, False)[1] for p in schedules))
        self.assertFalse(any(simulate(p)[1] for p in schedules))
        # 负控：去掉代际守卫，Resume 后过期票据仍可启动。
        stale = ("admit", "pause", "resume", "start")
        self.assertEqual(simulate(stale), (False, False))
        self.assertEqual(simulate(stale, check_revision=False), (True, True))
        self.assertEqual(simulate(("admit", "pause", "start")), (False, False))
        self.assertEqual(simulate(("admit", "pause", "start"), False, False), (True, True))

    def test_resume_command_replay(self):
        paused, revision, receipts = True, 1, {}
        receipts["resume-1"] = revision + 1
        paused, revision = False, 2
        paused, revision = True, 3  # 后续 Stop
        weak_paused = False  # 重试旧 Resume 再次应用
        if "resume-1" not in receipts:  # 新契约：已有回执只返回，不再修改
            paused = False
        self.assertFalse(weak_paused)
        self.assertTrue(paused)
        self.assertEqual(revision, 3)

    def test_lifecycle_reopen(self):
        old_request_epoch, current_epoch, gate_open = 1, 2, True
        self.assertTrue(gate_open)  # 弱 gate 会接纳旧请求
        self.assertFalse(gate_open and old_request_epoch == current_epoch)

    def test_unknown_store_commit(self):
        # 服务端已提交响应+续接，客户端收到 Unknown，不得按旧热态重新 Reason。
        durable = {"input": "satisfied", "continuation": "act"}
        cached_input = "pending"
        weak_reason = cached_input == "pending"
        mutation_result = "unknown"
        guarded_reason = cached_input == "pending" and mutation_result != "unknown"
        self.assertTrue(weak_reason)
        self.assertFalse(guarded_reason)
        self.assertEqual(durable["continuation"], "act")

    def test_missing_receipt_is_not_final_not_applied(self):
        # send → timeout → 查询无回执 → 延迟请求提交。
        receipt, delayed_request = None, True
        weak_replacement = receipt is None
        durable_responses = {"new-response"} if weak_replacement else set()
        if delayed_request:
            durable_responses.add("original-response")
        self.assertEqual(len(durable_responses), 2)
        # 强契约在没有持久终结证据时仍为 Unknown，不创建替代响应。
        terminal_rejection = False
        guarded_replacement = receipt is None and terminal_rejection
        self.assertFalse(guarded_replacement)

    def test_new_delegation_cannot_unpause_old_domain(self):
        domains = {"d1": {"generation": 1, "paused": True}}
        domains["d2"] = {"generation": 1, "paused": False}
        self.assertTrue(domains["d1"]["paused"])
        self.assertFalse(domains["d2"]["paused"])

    def test_single_instance_not_single_attempt(self):
        instance_claims = {"session": "instance"}
        self.assertEqual(len(instance_claims), 1)
        weak_attempts = ["user", "scan"]
        admitted = None
        guarded_attempts = []
        for source in weak_attempts:
            if admitted is None:  # 抽象为原子会话内准入，不证明真实实现原子性
                admitted = source
                guarded_attempts.append(source)
        self.assertEqual(len(weak_attempts), 2)
        self.assertEqual(len(guarded_attempts), 1)

    def test_withdraw_and_republish(self):
        input_id = "input"
        delivery = {(input_id, 1): "pending"}
        memory_queue = []  # 只撤内存，持久 pending 仍会被扫描
        self.assertFalse(memory_queue)
        self.assertIn("pending", delivery.values())
        delivery[(input_id, 1)] = "withdrawn"
        delivery[(input_id, 2)] = "pending"
        self.assertEqual([key for key, state in delivery.items() if state == "pending"],
                         [(input_id, 2)])

    def test_retry_budget_survives_attempts(self):
        limit = 2
        weak = sum(1 for _ in range(4) if 0 < limit)
        spent = 0
        for _ in range(4):
            if spent < limit:
                spent += 1
        self.assertGreater(weak, limit)
        self.assertEqual(spent, limit)

    def test_transcript_bypasses_scheduler_filter(self):
        entries = [("d1", "pending"), ("d2", "pending"), ("history", "completed")]
        paused = {"d1"}
        eligible_work = [d for d, state in entries if state == "pending" and d not in paused]
        self.assertEqual(eligible_work, ["d2"])
        weak_request = entries  # 只过滤调度，不过滤模型输入
        self.assertIn(("d1", "pending"), weak_request)
        guarded_request = [(d, s) for d, s in entries if s == "completed" or d in eligible_work]
        self.assertNotIn(("d1", "pending"), guarded_request)
        # 只证明结构过滤；不能证明 LLM 不会把历史背景当作新指令。

    def test_ack_does_not_survive_arbitrary_backup_rollback(self):
        inbox, owner_result = {"accepted": True}, True
        owner_result = False  # owner 在接纳后释放
        backup = {}  # 接纳前的备份
        inbox = backup
        self.assertFalse(inbox or owner_result)  # 责任及数据已丢失
        durable_nonrollback_log = {"accepted": True}
        self.assertTrue(durable_nonrollback_log)
        # 只有部署确实提供不回退日志时才可宣称该故障域内 RPO=0。


if __name__ == "__main__":
    unittest.main(verbosity=2)
