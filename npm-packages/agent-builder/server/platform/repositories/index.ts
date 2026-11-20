import type { Database } from 'bun:sqlite';
import { PlatformAgentsRepository } from './agents';
import { PlatformArtifactsRepository } from './artifacts';
import { PlatformAutomationInvocationsRepository, PlatformAutomationsRepository } from './automations';
import {
  PlatformEnvironmentFilesRepository,
  PlatformEnvironmentsRepository,
  PlatformEnvironmentTemplatesRepository,
} from './environments';
import {
  PlatformEventsRepository,
  PlatformItemsRepository,
  PlatformSessionsRepository,
  PlatformTurnsRepository,
} from './sessions';
import { PlatformSubagentsRepository } from './subagents';
import { PlatformTracesRepository } from './traces';
import { PlatformCredentialsRepository, PlatformVaultsRepository } from './vaults';

export interface PlatformRepositories {
  readonly agents: PlatformAgentsRepository;
  readonly sessions: PlatformSessionsRepository;
  readonly turns: PlatformTurnsRepository;
  readonly items: PlatformItemsRepository;
  readonly events: PlatformEventsRepository;
  readonly subagents: PlatformSubagentsRepository;
  readonly vaults: PlatformVaultsRepository;
  readonly credentials: PlatformCredentialsRepository;
  readonly environments: PlatformEnvironmentsRepository;
  readonly environmentTemplates: PlatformEnvironmentTemplatesRepository;
  readonly environmentFiles: PlatformEnvironmentFilesRepository;
  readonly automations: PlatformAutomationsRepository;
  readonly automationInvocations: PlatformAutomationInvocationsRepository;
  readonly artifacts: PlatformArtifactsRepository;
  readonly traces: PlatformTracesRepository;
}

export function createPlatformRepositories(db: Database): PlatformRepositories {
  return {
    agents: new PlatformAgentsRepository(db),
    sessions: new PlatformSessionsRepository(db),
    turns: new PlatformTurnsRepository(db),
    items: new PlatformItemsRepository(db),
    events: new PlatformEventsRepository(db),
    subagents: new PlatformSubagentsRepository(db),
    vaults: new PlatformVaultsRepository(db),
    credentials: new PlatformCredentialsRepository(db),
    environments: new PlatformEnvironmentsRepository(db),
    environmentTemplates: new PlatformEnvironmentTemplatesRepository(db),
    environmentFiles: new PlatformEnvironmentFilesRepository(db),
    automations: new PlatformAutomationsRepository(db),
    automationInvocations: new PlatformAutomationInvocationsRepository(db),
    artifacts: new PlatformArtifactsRepository(db),
    traces: new PlatformTracesRepository(db),
  };
}
