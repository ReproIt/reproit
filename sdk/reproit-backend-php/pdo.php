<?php

/*!
 * PDO wrap for reproit-backend-php: the one canonical database boundary,
 * emitting exactly the wire shape the Node reference records for the pg
 * driver: `pg` exchanges with request `{text, values}` and response
 * `{command, rowCount, rows}` or `{error: {message, code}}`.
 *
 * `RecordingPdo` extends \PDO. Capture: every statement executed through
 * `prepare`/`query`/`exec` is recorded on the ambient trace; result rows are
 * fetched once at execute time (associative, bounded at MAX_DB_ROWS) and
 * re-served to the application through the wrapped statement's own fetch
 * surface, so the app sees exactly the rows the driver returned. Replay
 * (`REPROIT_REPLAY`): the constructor is a CONNECT STUB, the parent PDO is
 * never initialized and no server is dialed, so the app boots with the
 * database down; statements are served from the recorded exchanges, a
 * recorded error re-raises as \PDOException, and a statement the capture
 * never saw raises DivergenceError (fail closed, marker on stderr).
 *
 * Only string statements through this surface are recorded; exotic paths
 * (bindParam streams, cursors, PDO::FETCH_INTO) pass through unrecorded in
 * capture mode and fail loudly in replay, never half-recorded.
 */

declare(strict_types=1);

namespace ReproitBackend;

require_once __DIR__ . '/exchange.php';
require_once __DIR__ . '/instrument.php';
require_once __DIR__ . '/replay.php';

/** Uppercased first word of a statement: the `command` tag ("SELECT"). */
function statement_command(string $text): string
{
    $trimmed = ltrim($text);
    $space = strcspn($trimmed, " \t\r\n");
    return strtoupper(substr($trimmed, 0, $space));
}

/** Record one statement outcome as a `pg` exchange on the ambient trace. */
function record_pg(string $text, ?array $values, array $outcome): void
{
    $request = ['text' => $text];
    if ($values !== null && $values !== []) {
        $request['values'] = array_values($values);
    }
    Instrument::record(
        statement_effect_kind($text),
        'pg',
        substr($text, 0, 256),
        ['protocol' => 'pg', 'request' => $request, 'response' => $outcome]
    );
}

/**
 * Match one statement against the replay session. Returns the recorded
 * response; raises on divergence or re-raises a recorded driver error.
 */
function serve_pg(ReplaySession $session, string $text, ?array $values): array
{
    $probe = ['text' => $text];
    if ($values !== null && $values !== []) {
        $probe['values'] = array_values($values);
    }
    $recorded = $session->match('pg', $probe);
    if ($recorded === null) {
        throw new DivergenceError('reproit: pg call diverged from the capture');
    }
    $outcome = $recorded['response'] ?? [];
    if (isset($outcome['error'])) {
        $raised = new \PDOException(
            (string) ($outcome['error']['message'] ?? 'recorded pg error')
        );
        $raised->errorInfo = [(string) ($outcome['error']['code'] ?? ''), null, null];
        throw $raised;
    }
    return $outcome;
}

final class RecordingPdo extends \PDO
{
    private bool $replaying;

    /**
     * The connect stub: in replay mode the parent \PDO is never initialized
     * and no server is dialed, so the application boots with the database
     * down. Anything reaching an un-overridden native method in replay fails
     * loudly ("PDO object is not initialized"), never half-works.
     */
    public function __construct(
        string $dsn,
        ?string $username = null,
        ?string $password = null,
        ?array $options = null,
    ) {
        $this->replaying = Instrument::replaying();
        if (!$this->replaying) {
            parent::__construct($dsn, $username, $password, $options);
        }
    }

    /** @return RecordingPdoStatement */
    #[\ReturnTypeWillChange]
    public function prepare(string $query, array $options = [])
    {
        if ($this->replaying) {
            return new RecordingPdoStatement($query, null);
        }
        try {
            $inner = parent::prepare($query, $options);
        } catch (\PDOException $error) {
            // A statement the driver rejects at prepare is still an exchange
            // the capsule must carry, or replay could not re-raise it.
            record_pg($query, null, ['error' => [
                'message' => $error->getMessage(),
                'code' => (string) ($error->errorInfo[0] ?? '') !== ''
                    ? (string) $error->errorInfo[0]
                    : null,
            ]]);
            throw $error;
        }
        if ($inner === false) {
            throw new \PDOException('prepare failed: ' . $query);
        }
        return new RecordingPdoStatement($query, $inner);
    }

    /** @return RecordingPdoStatement */
    #[\ReturnTypeWillChange]
    public function query(string $query, ?int $fetchMode = null, mixed ...$fetchModeArgs)
    {
        $statement = $this->prepare($query);
        $statement->execute();
        return $statement;
    }

    #[\ReturnTypeWillChange]
    public function exec(string $statement): int
    {
        $prepared = $this->prepare($statement);
        $prepared->execute();
        return $prepared->rowCount();
    }

    public function beginTransaction(): bool
    {
        return $this->replaying ? true : parent::beginTransaction();
    }

    public function commit(): bool
    {
        return $this->replaying ? true : parent::commit();
    }

    public function rollBack(): bool
    {
        return $this->replaying ? true : parent::rollBack();
    }
}

/**
 * The statement surface applications actually use, backed by rows fetched
 * once at execute time (capture) or served from the capsule (replay).
 * Minimal on purpose; anything else fails loudly.
 */
final class RecordingPdoStatement
{
    /** @var list<array> */
    private array $rows = [];
    private int $at = 0;
    private int $count = 0;

    public function __construct(
        private readonly string $text,
        private readonly ?\PDOStatement $inner,
    ) {
    }

    public function execute(?array $params = null): bool
    {
        $session = Instrument::session();
        if ($session !== null) {
            $outcome = serve_pg($session, $this->text, $params);
            $rows = \is_array($outcome['rows'] ?? null) ? $outcome['rows'] : [];
            $this->rows = array_values($rows);
            $this->at = 0;
            $this->count = \is_int($outcome['rowCount'] ?? null)
                ? $outcome['rowCount']
                : \count($this->rows);
            return true;
        }
        if ($this->inner === null) {
            throw new \LogicException('reproit: statement has no driver outside replay');
        }
        try {
            $this->inner->execute($params);
        } catch (\Throwable $error) {
            $code = $error instanceof \PDOException
                ? (string) ($error->errorInfo[0] ?? $error->getCode())
                : (string) $error->getCode();
            record_pg($this->text, $params, ['error' => [
                'message' => $error->getMessage(),
                'code' => $code === '' || $code === '0' ? null : $code,
            ]]);
            throw $error;
        }
        // Fetch once, serve from the stash: the recorded rows and the rows
        // the application observes are the same bytes by construction.
        $rows = $this->inner->columnCount() > 0
            ? $this->inner->fetchAll(\PDO::FETCH_ASSOC)
            : [];
        $this->rows = array_values(\is_array($rows) ? $rows : []);
        $this->at = 0;
        $driverCount = $this->inner->rowCount();
        $this->count = $this->rows !== [] ? \count($this->rows) : max(0, $driverCount);
        record_pg($this->text, $params, db_outcome([
            'command' => statement_command($this->text),
            'rowCount' => $this->count,
            'rows' => $this->rows,
        ]));
        return true;
    }

    /** Next row as an associative array, or false when drained. */
    public function fetch(int $mode = \PDO::FETCH_ASSOC): array|false
    {
        if ($this->at >= \count($this->rows)) {
            return false;
        }
        $row = $this->rows[$this->at];
        $this->at += 1;
        return $row;
    }

    /** @return list<array> */
    public function fetchAll(int $mode = \PDO::FETCH_ASSOC): array
    {
        $rest = \array_slice($this->rows, $this->at);
        $this->at = \count($this->rows);
        return $rest;
    }

    public function fetchColumn(int $column = 0): mixed
    {
        $row = $this->fetch();
        if ($row === false) {
            return false;
        }
        $values = array_values($row);
        return $values[$column] ?? false;
    }

    public function rowCount(): int
    {
        return $this->count;
    }

    public function closeCursor(): bool
    {
        $this->at = \count($this->rows);
        return $this->inner === null ? true : $this->inner->closeCursor();
    }
}
