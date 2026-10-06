import type { Transport } from "../transport/types";
import type { DomainWorkCommand, DomainWorkResolution, EntryResolution, ExecutionDomainPort, ExecutionReply, ExecutionTicket, WorkQuery } from "./types";

export class AcpExecutionDomain implements ExecutionDomainPort {
    constructor(private readonly transport: Transport) {}
    queryWork(sessionId: string): Promise<WorkQuery> {
        return this.transport.request("session/work/query", { sessionId });
    }
    execute(ticket: ExecutionTicket): Promise<ExecutionReply> {
        return this.transport.request("session/execute", { sessionId: ticket.sessionId, ticket });
    }
    resolveExecution(ticket: ExecutionTicket): Promise<EntryResolution> {
        return this.transport.request("session/execute/resolve", { sessionId: ticket.sessionId, ticket });
    }
    resolveWorkCommand(command: DomainWorkCommand): Promise<DomainWorkResolution> {
        return this.transport.request("session/work/resolve", { sessionId: command.sessionId, command });
    }
}
