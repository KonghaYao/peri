import type { ChatStateSnapshot } from "../src/entries/view";
import { ChatState } from "../src/entries/view";

/**
 * Compile-time negative fixture for the public snapshot contract.
 *
 * Functions are exported only so TypeScript checks their bodies; no test executes them. If
 * `ChatStateSnapshot` regresses to a shallow `Readonly`, the unused-expect-error diagnostics fail
 * `tsconfig.type-tests.json`.
 */
export function readonlySnapshotTypeChecks(snapshot: ChatStateSnapshot): void {
    // @ts-expect-error entries is a readonly array
    snapshot.entries.push(snapshot.entries[0]!);
    // @ts-expect-error entry fields are readonly
    snapshot.entries[0]!.role = "user";
    // @ts-expect-error entry arrays are readonly
    snapshot.entries[0]!.blocks.splice(0, 1);
    const block = snapshot.entries[0]!.blocks[0]!;
    if (block.type === "text") {
        // @ts-expect-error text block fields are readonly
        block.text = "changed";
    }
    // @ts-expect-error turns is a readonly array
    snapshot.turns.splice(0, 1);
    // @ts-expect-error turn fields are readonly
    snapshot.turns[0]!.status = "completed";
    // @ts-expect-error messages is a readonly array
    snapshot.messages.push({ entryId: "x", role: "user", text: "x" });
    // @ts-expect-error message fields are readonly
    snapshot.messages[0]!.text = "changed";
    // @ts-expect-error nested session fields are readonly
    snapshot.session.title = "changed";
    // @ts-expect-error nested activeTurn fields are readonly
    snapshot.activeTurn.turnId = "other";
    // @ts-expect-error interactions is a readonly array
    snapshot.interactions.push(snapshot.interactions[0]!);
}

export function readonlySubscribeTypeChecks(state: ChatState): void {
    state.subscribe((current) => {
        // @ts-expect-error subscribe exposes the same deeply readonly snapshot
        current.entries[0]!.role = "user";
    });
}
