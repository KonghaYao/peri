import type { Session } from "./session";

export class SendReceipt implements PromiseLike<void> {
    readonly inputId = crypto.randomUUID();
    private readonly delivery: Promise<void>;
    private readonly queued: Promise<void>;
    private forced?: Promise<void>;
    private sent = false;

    constructor(
        private readonly session: Session,
        text: string,
    ) {
        session.trackInput(this.inputId, text);
        this.delivery = session.waitForDelivery(this.inputId, () =>
            this.queued.then(() => session.dispatch(this.inputId)),
        ).then(() => {
            this.sent = true;
        });
        this.queued = session.enqueue(this.inputId, text).then(
            (state) => {
                if (state === "delivered")
                    session.confirmDelivery(this.inputId);
            },
            (error) => {
                session.rejectDelivery(this.inputId, error);
                throw error;
            },
        );
        // A caller may await only delivery; keep a failed queue command observed.
        void this.queued.catch(() => {});
    }

    get isSent(): boolean {
        return this.sent;
    }

    forceSend(): Promise<void> {
        this.forced ??= this.queued.then(() =>
            this.session.dispatch(this.inputId),
        );
        void this.forced.catch(() => {});
        return this.forced;
    }

    then<TResult1 = void, TResult2 = never>(
        onfulfilled?:
            | ((value: void) => TResult1 | PromiseLike<TResult1>)
            | null,
        onrejected?:
            | ((reason: unknown) => TResult2 | PromiseLike<TResult2>)
            | null,
    ): Promise<TResult1 | TResult2> {
        return this.delivery.then(onfulfilled, onrejected);
    }
}
