WITH owned AS (
    SELECT c.oid, c.relname
    FROM pg_catalog.pg_class c
    JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
    WHERE n.nspname = current_schema() AND c.relkind = 'r'
      AND c.relname::text = ANY(string_to_array($1, ','))
), facts AS (
    SELECT t.relname, 'column' AS kind, a.attname::text AS name,
           jsonb_build_array(a.attnum, format_type(a.atttypid, a.atttypmod),
                             a.attnotnull, a.attidentity, pg_get_expr(d.adbin, d.adrelid)) AS value
    FROM owned t
    JOIN pg_catalog.pg_attribute a ON a.attrelid = t.oid
    LEFT JOIN pg_catalog.pg_attrdef d ON d.adrelid = t.oid AND d.adnum = a.attnum
    WHERE a.attnum > 0 AND NOT a.attisdropped
    UNION ALL
    SELECT t.relname, 'constraint', con.conname::text, to_jsonb(pg_get_constraintdef(con.oid))
    FROM owned t JOIN pg_catalog.pg_constraint con ON con.conrelid = t.oid
    WHERE con.contype IN ('p', 'u', 'f', 'c')
    UNION ALL
    SELECT t.relname, 'index', idx.relname::text,
           jsonb_build_array(i.indisvalid, i.indisunique, i.indkey::text, i.indoption::text,
                             pg_get_expr(i.indpred, i.indrelid))
    FROM owned t
    JOIN pg_catalog.pg_index i ON i.indrelid = t.oid
    JOIN pg_catalog.pg_class idx ON idx.oid = i.indexrelid
    WHERE idx.relname::text = ANY(string_to_array($1, ','))
)
SELECT md5(jsonb_agg(jsonb_build_array(relname, kind, name, value)
                     ORDER BY relname COLLATE "C", kind COLLATE "C", name COLLATE "C")::text)
FROM facts
