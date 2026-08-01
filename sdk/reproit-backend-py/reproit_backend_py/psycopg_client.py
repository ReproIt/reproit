"""psycopg (v3) driver wrap: the one canonical DB driver, like pg in Node.

`wrap(psycopg)` patches `Cursor.execute` (and `AsyncCursor.execute` when
present) so every statement and its result are recorded as a `pg` exchange on
the ambient trace, exactly the wire shape the Node reference emits for the pg
driver: request `{text, values}`, response `{command, rowCount, rows}` or
`{error: {message, code}}`. Result rows are fetched once at execute time,
bounded at MAX_DB_ROWS, and re-served to the application through the wrapped
fetch methods, so the app's own fetchone/fetchall calls see exactly the rows
the driver returned.

With REPROIT_REPLAY set, `psycopg.connect` (and `AsyncConnection.connect`)
return in-process stubs served from the recorded exchanges: no server is
dialed, a recorded error re-raises, and a statement the capture never saw
raises DivergedError (fail closed, marker on stderr via the session).

Only the (query, params) shapes with str/bytes statements are recorded;
exotic forms (sql.SQL composables, COPY, server-side cursors) pass through
unrecorded rather than half-recorded, matching the Node wrapPg decision.
psycopg2 is NOT covered: a named capability gap, not a silent downgrade.
"""

from .instrument import (
    MAX_DB_ROWS,
    DivergedError,
    _STATE,
    _db_effect_kind,
    record_exchange,
)
from .trace import current_trace


def _statement_text(query):
    if isinstance(query, str):
        return query
    if isinstance(query, (bytes, bytearray)):
        return bytes(query).decode("utf-8", errors="replace")
    return None


def _probe(text, params):
    probe = {"text": text}
    if params:
        probe["values"] = list(params)
    return probe


def _record(trace, text, params, outcome):
    try:
        record_exchange(
            trace,
            _db_effect_kind(text),
            "pg",
            text[:256],
            {"protocol": "pg", "request": _probe(text, params), "response": outcome},
        )
    except Exception:
        _STATE["stats"]["failed_captures"] += 1


def _command(cursor):
    status = getattr(cursor, "statusmessage", None)
    if isinstance(status, str) and status:
        return status.split(" ")[0]
    return None


def _outcome(cursor, rows):
    recorded = [list(row) if isinstance(row, (list, tuple)) else row for row in rows]
    count = getattr(cursor, "rowcount", -1)
    outcome = {
        "command": _command(cursor),
        "rowCount": count if isinstance(count, int) and count >= 0 else len(rows),
        "rows": recorded[:MAX_DB_ROWS],
    }
    if len(recorded) > MAX_DB_ROWS:
        outcome["truncated"] = True
    return outcome


def _served_rows(outcome):
    """Recorded rows back in driver shape: JSON lists were tuples at capture
    (the psycopg default row factory), so they are served as tuples; dict
    rows (dict_row captures) stay dicts."""
    rows = outcome.get("rows") or []
    return [tuple(row) if isinstance(row, list) else row for row in rows]


def _serve(session, text, params):
    """Match one statement against the replay session. Returns the rows to
    stash; raises on divergence or a recorded error."""
    recorded = session.match("pg", _probe(text, params))
    if recorded is None:
        raise DivergedError("reproit: pg call diverged from the capture")
    outcome = recorded.get("response") or {}
    error = outcome.get("error")
    if error:
        raised = RuntimeError(str(error.get("message") or "recorded pg error"))
        raised.sqlstate = error.get("code")
        raise raised
    return outcome


class _Stash:
    """Rows fetched at record time (or served at replay), re-served through
    the cursor's fetch surface in driver order."""

    def __init__(self, rows):
        self.rows = list(rows)
        self.at = 0

    def one(self):
        if self.at >= len(self.rows):
            return None
        row = self.rows[self.at]
        self.at += 1
        return row

    def many(self, size):
        taken = self.rows[self.at : self.at + max(0, size)]
        self.at += len(taken)
        return taken

    def all(self):
        taken = self.rows[self.at :]
        self.at = len(self.rows)
        return taken


def _wrap_cursor_class(cursor_cls, is_async):
    # Idempotent at the class too: two wrapped module objects can share one
    # cursor class, and a double wrap would record every statement twice.
    if getattr(cursor_cls, "_reproit_wrapped", False):
        return
    cursor_cls._reproit_wrapped = True
    original_execute = cursor_cls.execute
    original_fetchone = cursor_cls.fetchone
    original_fetchmany = cursor_cls.fetchmany
    original_fetchall = cursor_cls.fetchall

    def _capture_result(self, text, params):
        trace = current_trace()
        if trace is None:
            return
        rows = []
        if getattr(self, "description", None) is not None:
            try:
                # Probe stash writability BEFORE consuming the result: a
                # cursor we cannot stash on must not be drained, or the app
                # would see an empty result the driver never returned.
                self._reproit_stash = None
                rows = list(original_fetchall(self))
                self._reproit_stash = _Stash(rows)
            except Exception:
                # Unstashable or unfetchable results record without rows; the
                # app keeps the cursor exactly as the driver left it.
                rows = []
        _record(trace, text, params, _outcome(self, rows))

    def _record_error(self, text, params, error):
        trace = current_trace()
        if trace is None:
            return
        _record(
            trace,
            text,
            params,
            {
                "error": {
                    "message": str(error),
                    "code": getattr(error, "sqlstate", None),
                }
            },
        )

    if is_async:

        async def _capture_result_async(self, text, params):
            trace = current_trace()
            if trace is None:
                return
            rows = []
            if getattr(self, "description", None) is not None:
                try:
                    self._reproit_stash = None
                    rows = list(await original_fetchall(self))
                    self._reproit_stash = _Stash(rows)
                except Exception:
                    rows = []
            _record(trace, text, params, _outcome(self, rows))

        async def execute(self, query, params=None, **kwargs):
            text = _statement_text(query)
            session = _STATE["session"]
            if session is not None and text is not None:
                self._reproit_stash = _Stash(_served_rows(_serve(session, text, params)))
                return self
            if text is None or current_trace() is None:
                return await original_execute(self, query, params, **kwargs)
            try:
                result = await original_execute(self, query, params, **kwargs)
            except Exception as error:
                _record_error(self, text, params, error)
                raise
            await _capture_result_async(self, text, params)
            return result

        async def fetchone(self):
            stash = getattr(self, "_reproit_stash", None)
            if stash is None:
                return await original_fetchone(self)
            return stash.one()

        async def fetchmany(self, size=0):
            stash = getattr(self, "_reproit_stash", None)
            if stash is None:
                return await original_fetchmany(self, size)
            return stash.many(size)

        async def fetchall(self):
            stash = getattr(self, "_reproit_stash", None)
            if stash is None:
                return await original_fetchall(self)
            return stash.all()

    else:

        def execute(self, query, params=None, **kwargs):
            text = _statement_text(query)
            session = _STATE["session"]
            if session is not None and text is not None:
                self._reproit_stash = _Stash(_served_rows(_serve(session, text, params)))
                return self
            if text is None or current_trace() is None:
                return original_execute(self, query, params, **kwargs)
            try:
                result = original_execute(self, query, params, **kwargs)
            except Exception as error:
                _record_error(self, text, params, error)
                raise
            _capture_result(self, text, params)
            return result

        def fetchone(self):
            stash = getattr(self, "_reproit_stash", None)
            if stash is None:
                return original_fetchone(self)
            return stash.one()

        def fetchmany(self, size=0):
            stash = getattr(self, "_reproit_stash", None)
            if stash is None:
                return original_fetchmany(self, size)
            return stash.many(size)

        def fetchall(self):
            stash = getattr(self, "_reproit_stash", None)
            if stash is None:
                return original_fetchall(self)
            return stash.all()

    cursor_cls.execute = execute
    cursor_cls.fetchone = fetchone
    cursor_cls.fetchmany = fetchmany
    cursor_cls.fetchall = fetchall


class _ReplayCursor:
    """Cursor served entirely from the capture. Minimal on purpose: the fetch
    surface plus lifecycle; anything else fails loudly."""

    def __init__(self, session):
        self._session = session
        self._stash = _Stash([])
        self.rowcount = -1
        self.statusmessage = None
        self.description = None

    def execute(self, query, params=None, **kwargs):
        text = _statement_text(query)
        if text is None:
            raise TypeError("reproit replay: only str/bytes statements are replayable")
        outcome = _serve(self._session, text, params)
        self._stash = _Stash(_served_rows(outcome))
        self.rowcount = outcome.get("rowCount", len(self._stash.rows))
        self.statusmessage = outcome.get("command")
        self.description = () if self._stash.rows else None
        return self

    def fetchone(self):
        return self._stash.one()

    def fetchmany(self, size=0):
        return self._stash.many(size)

    def fetchall(self):
        return self._stash.all()

    def __iter__(self):
        return iter(self._stash.all())

    def close(self):
        return None

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()


class _ReplayConnection:
    """Connection stub for hermetic replay: no server is ever dialed."""

    def __init__(self, session):
        self._session = session
        self.autocommit = False
        self.closed = False

    def cursor(self, *args, **kwargs):
        return _ReplayCursor(self._session)

    def execute(self, query, params=None, **kwargs):
        return self.cursor().execute(query, params, **kwargs)

    def commit(self):
        return None

    def rollback(self):
        return None

    def close(self):
        self.closed = True

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()


class _AsyncReplayCursor(_ReplayCursor):
    async def execute(self, query, params=None, **kwargs):  # type: ignore[override]
        return _ReplayCursor.execute(self, query, params, **kwargs)

    async def fetchone(self):  # type: ignore[override]
        return self._stash.one()

    async def fetchmany(self, size=0):  # type: ignore[override]
        return self._stash.many(size)

    async def fetchall(self):  # type: ignore[override]
        return self._stash.all()

    async def __aenter__(self):
        return self

    async def __aexit__(self, *exc):
        return None


class _AsyncReplayConnection(_ReplayConnection):
    def cursor(self, *args, **kwargs):
        return _AsyncReplayCursor(self._session)

    async def execute(self, query, params=None, **kwargs):  # type: ignore[override]
        return await self.cursor().execute(query, params, **kwargs)

    async def commit(self):  # type: ignore[override]
        return None

    async def rollback(self):  # type: ignore[override]
        return None

    async def close(self):  # type: ignore[override]
        self.closed = True

    async def __aenter__(self):
        return self

    async def __aexit__(self, *exc):
        self.closed = True


def wrap(psycopg):
    """Patch the module (or module-shaped object). Idempotent."""
    if psycopg is None or getattr(psycopg, "_reproit_wrapped", False):
        return psycopg
    cursor_cls = getattr(psycopg, "Cursor", None)
    if cursor_cls is not None:
        _wrap_cursor_class(cursor_cls, is_async=False)
    async_cursor_cls = getattr(psycopg, "AsyncCursor", None)
    if async_cursor_cls is not None:
        _wrap_cursor_class(async_cursor_cls, is_async=True)

    original_connect = getattr(psycopg, "connect", None)
    if original_connect is not None:

        def connect(*args, **kwargs):
            # Hermetic replay never dials: the stub serves the capture.
            if _STATE["session"] is not None:
                return _ReplayConnection(_STATE["session"])
            return original_connect(*args, **kwargs)

        psycopg.connect = connect

    async_conn_cls = getattr(psycopg, "AsyncConnection", None)
    if async_conn_cls is not None:
        original_aconnect = async_conn_cls.connect

        async def aconnect(cls, *args, **kwargs):
            if _STATE["session"] is not None:
                return _AsyncReplayConnection(_STATE["session"])
            return await original_aconnect(*args, **kwargs)

        async_conn_cls.connect = classmethod(aconnect)

    try:
        psycopg._reproit_wrapped = True
    except AttributeError:
        pass
    return psycopg
