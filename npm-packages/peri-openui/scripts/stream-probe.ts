import { createParser } from "@openuidev/react-lang";
import { openuiLibrary } from "../src/library";

const schema = openuiLibrary.toJSONSchema();
const root = openuiLibrary.root;

function freshParse(text: string) {
  return createParser(schema, root).parse(text) as unknown as {
    root?: unknown;
    meta?: { incomplete?: boolean; errors?: unknown[]; unresolved?: unknown[] };
  };
}

const rootFirst = `root = Stack([title, table])
title = Text("标题")
table = Table(["A", "B"], [["1", "2"]])`;

const rootLast = `title = Text("标题")
table = Table(["A", "B"], [["1", "2"]])
root = Stack([title, table])`;

for (const [name, full] of [["rootFirst", rootFirst], ["rootLast", rootLast]] as const) {
  console.log("=== ", name);
  let last = "";
  for (let i = 0; i <= full.length; i += 1) {
    const p = freshParse(full.slice(0, i));
    const s = {
      i,
      root: Boolean(p?.root),
      incomplete: p?.meta?.incomplete ?? null,
      errors: p?.meta?.errors?.length ?? null,
      unresolved: p?.meta?.unresolved?.length ?? null,
    };
    const key = JSON.stringify(s);
    if (key !== last) {
      console.log(key, JSON.stringify(full.slice(0, i).split("\n").at(-1)?.slice(-24)));
      last = key;
    }
  }
}

const reused = createParser(schema, root);
for (const n of [1, 1, 2]) {
  const p = reused.parse(rootLast) as unknown as {
    meta?: { incomplete?: boolean; errors?: unknown[]; unresolved?: unknown[] };
    statements?: unknown[];
  };
  console.log("reuse", n, {
    incomplete: p?.meta?.incomplete,
    errors: p?.meta?.errors?.length,
    unresolved: p?.meta?.unresolved?.length,
    statements: p?.statements?.length,
  });
}
