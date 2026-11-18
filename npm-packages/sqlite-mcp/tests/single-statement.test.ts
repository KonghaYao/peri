import assert from 'node:assert/strict';
import test from 'node:test';
import {
  assertNoExternalFileTarget,
  assertSingleStatement,
  ExternalFileTargetError,
  SingleStatementError,
} from '../src/single-statement.js';

const accepted = [
  ['SELECT 1', '无分号的单条语句'],
  ['SELECT 1;', '结尾分号'],
  ['SELECT 1; -- trailing comment', '尾随行注释'],
  ['SELECT 1; /* trailing block */', '尾随块注释'],
  ["SELECT ';' AS value", '字符串里的分号'],
  ["SELECT 'it''s; fine' AS value", '转义单引号后的分号'],
  ['-- comment ;\nSELECT 1', '前置注释里的分号'],
  ['SELECT 1; -- comment\rSELECT 2', '裸 CR 不结束行注释'],
  ['/* ; */ SELECT 1', '前置块注释里的分号'],
  [
    `CREATE TRIGGER audit_items AFTER INSERT ON items BEGIN
       INSERT INTO audit(message) VALUES ('one;two');
       INSERT INTO audit(message) VALUES (CASE WHEN new.value IS NULL THEN 'null' ELSE new.value END);
     END`,
    '单条触发器体里的多个分号与 CASE END',
  ],
  [
    `CREATE TRIGGER source_audit AFTER INSERT ON items BEGIN
       INSERT INTO audit(message) SELECT END标记 FROM source;
     END`,
    '触发器里的 Unicode 标识符 END标记',
  ],
  [`CREATE TEMP TRIGGER t AFTER INSERT ON items BEGIN INSERT INTO audit VALUES ('END;'); END; /* ok */`, '临时触发器'],
] as const;

for (const [sql, label] of accepted) {
  test(`accepts ${label}`, () => {
    assert.doesNotThrow(() => assertSingleStatement(sql));
  });
}

const rejected = [
  ['', '空输入'],
  ['  -- only a comment\n', '只有注释'],
  ['SELECT 1; SELECT 2', '两条查询'],
  ['INSERT INTO items VALUES (1); INSERT INTO items VALUES (2);', '两条写入'],
  ['CREATE TABLE a(x); CREATE TABLE b(y)', '两条 DDL'],
  ['; SELECT 1', '前置空语句'],
  ['SELECT 1;;', '重复分号'],
  ['SELECT 1; -- comment\r\nSELECT 2', 'CRLF 结束行注释后仍有语句'],
  [
    `CREATE TRIGGER t AFTER INSERT ON items BEGIN INSERT INTO audit SELECT END标记 FROM source; END; SELECT 1`,
    'Unicode 标识符触发器后仍有语句',
  ],
  [`CREATE TABLE "END标记" (x); SELECT 1`, 'Unicode 标识符表后仍有语句'],
] as const;

for (const [sql, label] of rejected) {
  test(`rejects ${label}`, () => {
    assert.throws(
      () => assertSingleStatement(sql),
      (error: unknown) => error instanceof SingleStatementError && !String(error.message).includes('SELECT'),
    );
  });
}

test('external file target guard blocks only VACUUM INTO', () => {
  for (const sql of ["VACUUM INTO 'x'", "vacuum main INTO 'x'", "VACUUM/*c*/INTO 'x'", `VACUUM "main" INTO 'x'`]) {
    assert.throws(
      () => assertNoExternalFileTarget(sql),
      (error: unknown) => error instanceof ExternalFileTargetError,
      sql,
    );
  }

  for (const sql of [
    'VACUUM',
    'VACUUM main',
    'VACUUM my_into',
    "EXPLAIN VACUUM INTO 'x'",
    "EXPLAIN QUERY PLAN VACUUM INTO 'x'",
    "SELECT 'VACUUM INTO ''x'''",
    'SELECT 1 AS "VACUUM INTO"',
  ]) {
    assert.doesNotThrow(() => assertNoExternalFileTarget(sql), sql);
  }
});
