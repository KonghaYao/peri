/** Settings document accepted by Peri's trusted stdin bootstrap. */
export interface PeriConfig {
  $schema?: string;
  config: {
    active_alias?: "fable" | "opus" | "sonnet" | "haiku";
    providers?: Array<{
      id: string;
      type: "anthropic" | "openai";
      apiKey?: string;
      baseUrl?: string;
      name?: string;
      models?: Partial<Record<"fable" | "opus" | "sonnet" | "haiku", string>>;
    }>;
    profiles?: Partial<
      Record<
        "fable" | "opus" | "sonnet" | "haiku",
        {
          provider?: string;
          model?: string;
          effort?: string;
          max_tokens?: number;
          context_1m?: boolean;
        }
      >
    >;
    env?: Record<string, string>;
    language?: string;
    persona?: string;
    tone?: string;
    proactiveness?: string;
    show_cache_warning?: boolean;
    claude_md_excludes?: string[];
    meta_harness?: Partial<Record<MetaHarnessKey, boolean>>;
  };
}

/** Keys accepted by peri-acp-types/src/meta_harness.rs. */
export type MetaHarnessKey =
  | "01_intro"
  | "02_system"
  | "03_doing_tasks"
  | "04_actions"
  | "05_using_tools"
  | "06_tone_style"
  | "07_runtime"
  | "10_hitl"
  | "11_subagent"
  | "12_ask_user"
  | "13_skills"
  | "persona"
  | "language"
  | "DefaultSystemPromptMiddleware"
  | "LangMiddleware"
  | "AgentsMdMiddleware"
  | "PluginMiddleware"
  | "SkillsMiddleware"
  | "SkillPreloadMiddleware"
  | "AtMentionMiddleware"
  | "ImageMiddleware"
  | "GitAttributionMiddleware"
  | "TodoMiddleware"
  | "HookMiddleware"
  | "PermissionMiddleware"
  | "HumanInTheLoopMiddleware"
  | "SubAgentMiddleware"
  | "McpMiddleware"
  | "WorkflowMiddleware"
  | "ToolSearch"
  | "GoalMiddleware"
  | "WebMiddleware"
  | "ArtifactMiddleware"
  | "CronMiddleware"
  | "WorkspaceMiddleware"
  | "BuiltInSubagents";
