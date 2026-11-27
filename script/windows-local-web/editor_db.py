import base64
from contextlib import contextmanager
import re
import sqlite3


SQL_TOKENS = re.compile(
    r"""--[^
]*|/\*.*?\*/|'(?:''|[^'])*'|"(?:[^"]|"")*"|\[[^\]]*\]|`(?:[^`]|``)*`|(\?[0-9]*)|([:@$]\w+)""",
    re.DOTALL,
)


def statements(sql):
    start = 0
    for end, character in enumerate(sql, start=1):
        if character == ";" and sqlite3.complete_statement(sql[start:end]):
            yield sql[start:end]
            start = end
    if sql[start:].strip():
        yield sql[start:]


def binding_count(sql):
    count = 0
    named = set()
    for token in SQL_TOKENS.finditer(sql):
        positional, name = token.groups()
        if positional:
            count = count + 1 if positional == "?" else max(count, int(positional[1:]))
        elif name and name not in named:
            named.add(name)
            count += 1
    return count


def decode_parameter(value, previous_row_ids):
    if not isinstance(value, dict):
        return value
    match value.get("type"):
        case "blob":
            return base64.b64decode(value["data"], validate=True)
        case "batch_last_rowid":
            index = value["query"]
            if not isinstance(index, int) or not 0 <= index < len(previous_row_ids):
                raise ValueError("Invalid batch row ID reference")
            return previous_row_ids[index]
        case _:
            raise ValueError("Unsupported SQL parameter")


def encode_value(value):
    if isinstance(value, bytes):
        return {"type": "blob", "data": base64.b64encode(value).decode("ascii")}
    return value


def authorize(action, value, argument, _database, _trigger):
    if action in (sqlite3.SQLITE_ATTACH, sqlite3.SQLITE_DETACH):
        return sqlite3.SQLITE_DENY
    if action == sqlite3.SQLITE_FUNCTION and argument.lower() == "load_extension":
        return sqlite3.SQLITE_DENY
    if action == sqlite3.SQLITE_PRAGMA and value.lower() not in {
        "busy_timeout", "cache_size", "case_sensitive_like", "foreign_keys", "integrity_check", "journal_mode",
        "locking_mode", "quick_check", "recursive_triggers", "synchronous", "table_info",
        "temp_store", "user_version",
    }:
        return sqlite3.SQLITE_DENY
    if action == sqlite3.SQLITE_PRAGMA and value.lower() == "temp_store" and argument not in {
        None, "MEMORY", "memory", "2",
    }:
        return sqlite3.SQLITE_DENY
    return sqlite3.SQLITE_OK


class EditorDatabase:
    def __init__(self):
        self.reset()

    def reset(self):
        self.connection = sqlite3.connect(":memory:", autocommit=True)
        self.connection.execute("PRAGMA temp_store = MEMORY")
        self.connection.execute("PRAGMA foreign_keys = ON")
        self.connection.set_authorizer(authorize)
        self.connection.set_progress_handler(lambda: 1, 2_000_000)

    @contextmanager
    def transaction(self):
        self.connection.execute("SAVEPOINT gateway_batch")
        try:
            yield
        except Exception:
            self.connection.execute("ROLLBACK TO gateway_batch")
            raise
        finally:
            self.connection.execute("RELEASE gateway_batch")

    def query(self, request, previous_row_ids=()):
        arguments = [decode_parameter(value, previous_row_ids) for value in request.get("params", [])]
        before = self.connection.total_changes
        columns = []
        rows = []
        last_row_id = None
        parts = list(statements(request["sql"]))
        for part in parts:
            parameters = arguments if len(parts) == 1 else arguments[:binding_count(part)]
            cursor = self.connection.execute(part, parameters)
            last_row_id = cursor.lastrowid
            if cursor.description:
                columns = [column[0] for column in cursor.description]
                rows = [[encode_value(value) for value in row] for row in cursor]
        return {
            "columns": columns,
            "rows": rows,
            "last_rowid": last_row_id,
            "changes": self.connection.total_changes - before,
        }

    def script(self, sql):
        for statement in statements(sql):
            self.connection.execute(statement)

    def migrate(self, request):
        domain = request["domain"]
        migrations = [sql.strip() for sql in request["migrations"]]
        allowed = set(request.get("allowed_changes", []))
        with self.transaction():
            self.connection.execute(
                "CREATE TABLE IF NOT EXISTS migrations "
                "(domain TEXT, step INTEGER, migration TEXT, PRIMARY KEY (domain, step))"
            )
            saved = dict(self.connection.execute(
                "SELECT step, migration FROM migrations WHERE domain = ?", (domain,)
            ))
            changes = [
                {"index": index, "stored": saved[index], "proposed": sql}
                for index, sql in enumerate(migrations)
                if index in saved and saved[index] != sql and index not in allowed
            ]
            if changes:
                return {"status": "drift", "changes": changes}
            for index, sql in enumerate(migrations):
                if index not in saved:
                    self.script(sql)
                if saved.get(index) != sql:
                    self.connection.execute(
                        "INSERT OR REPLACE INTO migrations (domain, step, migration) VALUES (?, ?, ?)",
                        (domain, index, sql),
                    )
        return {"status": "ok"}

    def call(self, method, request):
        match method:
            case "Sql::query":
                return self.query(request)
            case "Sql::script":
                self.script(request["sql"])
            case "Sql::migrate":
                return self.migrate(request)
            case "Sql::reset":
                self.connection.close()
                self.reset()
            case "Sql::batch":
                previous_row_ids = []
                with self.transaction():
                    for query in request["queries"]:
                        previous_row_ids.append(self.query(query, previous_row_ids)["last_rowid"])
            case "Sql::bootstrap_kvp":
                return {
                    "kv": list(self.connection.execute("SELECT key, value FROM kv_store")),
                    "scoped": list(self.connection.execute(
                        "SELECT namespace, key, value FROM scoped_kv_store"
                    )),
                }
            case _:
                raise ValueError("Unknown editor database method")
