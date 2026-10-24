import { expect, test } from "bun:test";
import * as Y from "yjs";
import * as view from "../src/view/index";

test("the browser view entrypoint exposes ChatView/ChatReplica and shared Yjs identity", () => {
    expect(view.Y).toBe(Y);
    expect(view.Y.Doc).toBe(Y.Doc);
    expect(typeof view.ChatView).toBe("function");
    expect(typeof view.ChatReplica).toBe("function");
    expect(typeof view.readSessionView).toBe("function");
    expect(typeof view.selectEntry).toBe("function");
    expect("SessionViewStore" in view).toBe(false);
    expect("SessionDocs" in view).toBe(false);
});
