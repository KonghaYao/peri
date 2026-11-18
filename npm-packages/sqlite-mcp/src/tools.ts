import type { CallToolResult, MCPServer } from 'mcp-use';
import { z } from 'zod';
import {
  describeTable,
  executeSql,
  listTables,
  TableNotFoundError,
  type TableDescription,
  type TableInfo,
} from './database.js';
import { DatabaseFileError } from './database-file.js';
import { ExternalFileTargetError, SingleStatementError } from './single-statement.js';

const bindValueSchema = z.union([z.string(), z.number(), z.boolean(), z.null()]);

const cellSchema = z
  .union([
    z.string(),
    z.number(),
    z.boolean(),
    z.null(),
    z.object({ type: z.literal('integer'), value: z.string() }),
    z.object({ type: z.literal('real'), value: z.string() }),
    z.object({ type: z.literal('blob'), base64: z.string(), byteLength: z.number().int().nonnegative() }),
    z.object({ type: z.literal('unsupported'), valueType: z.string() }),
  ])
  .describe(
    '普通 JSON 标量；超出安全整数范围的 INTEGER 为 {type:"integer",value}，BLOB 为 {type:"blob",base64,byteLength}。',
  );

const executeSqlInput = z.object({
  sql: z.string().min(1).describe('恰好一条 SQLite 语句；可选按顺序绑定 ? 占位符。'),
  params: z.array(bindValueSchema).optional().describe('按出现顺序绑定 ? 的参数，支持 string/number/boolean/null。'),
});

const executeSqlOutput = z.object({
  kind: z.enum(['rows', 'none']).describe('rows 表示语句有结果列；none 表示无结果语句（写、DDL、无结果 PRAGMA 等）。'),
  columns: z.array(z.string()),
  rows: z.array(z.array(cellSchema)),
  returnedRowCount: z.number().int().nonnegative(),
  totalRowCount: z.number().int().nonnegative(),
  truncated: z.boolean().describe('只表示响应被截断；语句本身已完整执行。'),
  changes: z.number().int().nonnegative().optional().describe('无结果语句影响的行数。'),
});

const listTablesOutput = z.object({
  tables: z.array(z.object({ name: z.string(), type: z.enum(['table', 'view']) })),
});

const describeTableInput = z.object({
  table: z.string().min(1).describe('主数据库中的表或视图名称。'),
});

const describeTableOutput = z.object({
  name: z.string(),
  type: z.enum(['table', 'view']),
  sql: z.string().nullable(),
  columns: z.array(
    z.object({
      name: z.string(),
      type: z.string(),
      notNull: z.boolean(),
      primaryKey: z.boolean(),
      primaryKeyPosition: z.number().int().nonnegative(),
      defaultValue: z.string().nullable(),
    }),
  ),
});

function toolError(message: string): CallToolResult & { isError: true } {
  return { content: [{ type: 'text', text: message }], isError: true };
}

/** 只根据驱动错误文本分类，绝不回显原始 message（其中可能带 SQL 或数据库内容）。 */
function databaseErrorMessage(error: unknown): string {
  if (error instanceof SingleStatementError || error instanceof ExternalFileTargetError) return error.message;
  if (error instanceof DatabaseFileError) return '数据库文件不可用';
  const message = error instanceof Error ? error.message : '';
  if (/failed to open database|file is not a database/i.test(message)) return '数据库文件不可用';
  if (/no such table|no such column|no such function/i.test(message)) return 'SQL 执行失败：表或字段不存在';
  if (/constraint/i.test(message)) return 'SQL 执行失败：约束冲突';
  if (/syntax|parse error/i.test(message)) return 'SQL 执行失败：语法错误';
  if (/locked|busy/i.test(message)) return 'SQL 执行失败：数据库忙';
  if (/readonly|read-only/i.test(message)) return 'SQL 执行失败：数据库只读';
  return 'SQL 执行失败';
}

function schemaErrorMessage(error: unknown): string {
  if (error instanceof TableNotFoundError) return '未找到表或视图';
  if (error instanceof DatabaseFileError) return '数据库文件不可用';
  return '读取数据库结构失败';
}

function summarizeExecution(result: Awaited<ReturnType<typeof executeSql>>): string {
  if (result.kind === 'none') return `语句已执行，changes=${result.changes}`;
  return result.truncated
    ? `已返回 ${result.returnedRowCount} 行（共 ${result.totalRowCount} 行，响应已截断）`
    : `已返回 ${result.returnedRowCount} 行`;
}

export function registerTools(server: MCPServer, databasePath: string): void {
  server.tool(
    {
      name: 'execute_sql',
      title: '执行 SQL',
      description:
        '执行恰好一条 SQL 语句，可选按顺序绑定 ? 参数。每次调用使用独立连接，不保留跨调用事务；' +
        '结果最多返回 100 行并标记 truncated，SQL 本身不会被改写或加 LIMIT。',
      inputSchema: executeSqlInput,
      outputSchema: executeSqlOutput,
      annotations: { destructiveHint: true, openWorldHint: false },
    },
    async ({ sql, params }) => {
      try {
        for (const value of params ?? []) {
          if (typeof value === 'number' && !Number.isFinite(value)) return toolError('参数必须是有限数字');
        }
        const result = await executeSql(databasePath, sql, params ?? []);
        return {
          content: [{ type: 'text', text: summarizeExecution(result) }],
          structuredContent: result,
        };
      } catch (error) {
        return toolError(databaseErrorMessage(error));
      }
    },
  );

  server.tool(
    {
      name: 'list_tables',
      title: '列出表与视图',
      description: '列出主数据库中的用户表和视图；SQLite 内部 sqlite_ 对象不会出现。',
      inputSchema: z.object({}),
      outputSchema: listTablesOutput,
      annotations: { readOnlyHint: true, destructiveHint: false, openWorldHint: false },
    },
    async () => {
      try {
        const tables: TableInfo[] = await listTables(databasePath);
        return {
          content: [{ type: 'text', text: `${tables.length} 个表/视图` }],
          structuredContent: { tables },
        };
      } catch (error) {
        return toolError(schemaErrorMessage(error));
      }
    },
  );

  server.tool(
    {
      name: 'describe_table',
      title: '描述表或视图',
      description: '返回表或视图的字段、声明类型、主键位置，以及 sqlite_schema 中保存的原始定义 SQL。',
      inputSchema: describeTableInput,
      outputSchema: describeTableOutput,
      annotations: { readOnlyHint: true, destructiveHint: false, openWorldHint: false },
    },
    async ({ table }) => {
      try {
        const description: TableDescription = await describeTable(databasePath, table);
        return {
          content: [{ type: 'text', text: `${description.name}: ${description.columns.length} 个字段` }],
          structuredContent: description,
        };
      } catch (error) {
        return toolError(schemaErrorMessage(error));
      }
    },
  );
}
