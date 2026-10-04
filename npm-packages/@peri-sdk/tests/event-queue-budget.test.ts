import { expect, test } from "bun:test";
import { EventQueue } from "../src/transport/event-queue";
import { EventStreamOverflowError } from "../src/transport/notification-budget";

const notification = (text: string) => ({ jsonrpc: "2.0" as const, method: "session/update", params: { text } });

test("an unconsumed diagnostic stream fails explicitly at its retention budget", async () => {
    let detached = 0;
    const queue = new EventQueue(() => { detached++; });
    for (let i = 0; i < 1025; i++) queue.push(notification(String(i)));
    expect(detached).toBe(1);
    await expect(queue.next()).rejects.toBeInstanceOf(EventStreamOverflowError);
    await queue.return();
    expect(await queue.next()).toEqual({ value: undefined, done: true });
});

test("large queued events hit the byte budget and a waiting live consumer still receives large frames", async () => {
    const queue = new EventQueue(() => {});
    const large = notification("x".repeat(3 * 1024 * 1024));
    const waiting = queue.next(); queue.push(large);
    expect((await waiting).value).toBe(large);
    queue.push(large);
    await expect(queue.next()).rejects.toBeInstanceOf(EventStreamOverflowError);
});

test("draining and finishing a bounded queue preserves order and wakes consumers", async () => {
    const queue = new EventQueue(() => {});
    for (let i = 0; i < 1000; i++) queue.push(notification(String(i)));
    for (let i = 0; i < 1000; i++) expect((await queue.next()).value).toEqual(notification(String(i)));
    const waiting = queue.next(); queue.finish();
    expect(await waiting).toEqual({ done: true, value: undefined });
});
