-- oracle-monitor.sql — Oracle-side observability for the load test.
-- Run it **while a load test is in progress** (`just monitor oracle` in another
-- terminal): sections 2-4 read live `kora` sessions and are empty once Kora exits.
-- Connects as SYSTEM because the V$ views are not granted to the app user `kora`.
--   sqlplus -s system/oracle@localhost:1521/FREEPDB1 < oracle-monitor.sql

SET LINESIZE 200
SET PAGESIZE 200
SET FEEDBACK OFF
COLUMN name FORMAT A14
COLUMN value FORMAT A10
COLUMN event FORMAT A30
COLUMN sql_text FORMAT A60
COLUMN wait_class FORMAT A15

PROMPT == 1. open_cursors ceiling ==
SELECT name, value FROM v$parameter WHERE name = 'open_cursors';

PROMPT
PROMPT == 2. Open cursors per kora session ==
PROMPT ==    (the thick `oracle` driver closes cursors on statement drop, so
PROMPT ==    this stays low/stable — no leak to watch for) ==
SELECT s.sid, s.serial#, COUNT(*) AS open_cursors
FROM v$open_cursor oc
JOIN v$session s ON s.sid = oc.sid
WHERE s.username = 'KORA'
GROUP BY s.sid, s.serial#
ORDER BY open_cursors DESC;

PROMPT
PROMPT == 3. Active kora sessions ==
SELECT sid, serial#, status, event, sql_id
FROM v$session
WHERE username = 'KORA'
ORDER BY status, sid;

PROMPT
PROMPT == 4. Top SQL by elapsed time ==
SELECT * FROM (
  SELECT sql_id,
         executions AS execs,
         ROUND(elapsed_time / 1000) AS elapsed_ms,
         SUBSTR(sql_text, 1, 60) AS sql_text
  FROM v$sql
  ORDER BY elapsed_time DESC
) WHERE ROWNUM <= 15;

PROMPT
PROMPT == 5. Blocking sessions (lock contention) ==
SELECT blocking_session, sid, serial#, wait_class, event
FROM v$session
WHERE blocking_session IS NOT NULL;

EXIT
