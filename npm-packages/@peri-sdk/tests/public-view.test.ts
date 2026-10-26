import { expect, test } from "bun:test";
import * as Y from "yjs";
import * as view from "../src/view/index";
import * as sdk from "../src/sdk/index";

test("the browser view entrypoint exposes ChatView/ChatReplica and shared Yjs identity", () => {
    expect(view.Y).toBe(Y);
    expect(view.Y.Doc).toBe(Y.Doc);
    expect(typeof view.ChatView).toBe("function");
    expect(typeof view.ChatReplica).toBe("function");
    expect(typeof view.readSessionView).toBe("function");
    expect("selectEntry" in view).toBe(false);
    expect("SessionViewStore" in view).toBe(false);
    expect("SessionDocs" in view).toBe(false);
    // 宿主共享策略不属于纯浏览器视图面。
    expect("ChatViewRegistry" in view).toBe(false);
});

test("the main entrypoint exposes the Chat surface and no old execution facades", () => {
    expect(typeof sdk.ChatManager).toBe("function");
    expect(typeof sdk.ChatService).toBe("function");
    expect(typeof sdk.ChatUserAction).toBe("object");
    expect(typeof sdk.ChatView).toBe("function");
    expect(typeof sdk.ChatReplica).toBe("function");
    expect(typeof sdk.ChatViewRegistry).toBe("function");
    expect("Agent" in sdk).toBe(false);
    expect("ManagedAgents" in sdk).toBe(false);
    expect("SessionDocs" in sdk).toBe(false);
    expect("SessionViewStore" in sdk).toBe(false);
    expect("AtomicManagedAgentKv" in sdk).toBe(false);
    expect("AgentOptions" in sdk).toBe(false);
});
