"""PostgreSQL psql CLI helpers for isolated E2E test setup & verification."""

from __future__ import annotations

from datetime import date, datetime, timedelta, timezone
import hashlib
import json
import os
import subprocess
from typing import Any
from urllib.parse import urlparse


def sql_literal(value: str | None) -> str:
    if value is None:
        return "NULL"
    return "'" + value.replace("'", "''") + "'"


def sql_ident(name: str) -> str:
    return '"' + name.replace('"', '""') + '"'


class DbHelper:
    """Executes deterministic PostgreSQL resets, seeds, and read-only checks via psql."""

    def __init__(self, db_url: str, admin_db_url: str | None = None) -> None:
        self.db_url = db_url
        parsed = urlparse(db_url)
        self.db_name = parsed.path.lstrip("/") or "nsac_e2e"
        if admin_db_url is None:
            admin_db_url = parsed._replace(path="/postgres").geturl()
        self.admin_db_url = admin_db_url

    def _run_psql(
        self,
        sql: str,
        *,
        db_url: str | None = None,
        tuples_only: bool = False,
    ) -> str:
        target = db_url or self.db_url
        parsed = urlparse(target)
        host = parsed.hostname or "127.0.0.1"
        port = str(parsed.port or 5432)
        user = parsed.username or "postgres"
        dbname = parsed.path.lstrip("/") or "postgres"

        env = os.environ.copy()
        if parsed.password:
            env["PGPASSWORD"] = parsed.password

        cmd = [
            "psql",
            "-h",
            host,
            "-p",
            port,
            "-U",
            user,
            "-d",
            dbname,
            "-v",
            "ON_ERROR_STOP=1",
        ]
        if tuples_only:
            cmd.extend(["-t", "-A"])
        cmd.extend(["-c", sql])

        proc = subprocess.run(
            cmd,
            env=env,
            capture_output=True,
            text=True,
            check=False,
        )
        if proc.returncode != 0:
            raise RuntimeError(
                f"psql failed (rc={proc.returncode}): {proc.stderr.strip() or proc.stdout.strip()}"
            )
        return proc.stdout.strip()

    def check_connectivity(self) -> bool:
        out = self._run_psql("SELECT 1;", db_url=self.admin_db_url, tuples_only=True)
        return out == "1"

    def reset_database(self) -> None:
        db_lit = sql_literal(self.db_name)
        db_id = sql_ident(self.db_name)
        # Must run in separate psql invocations: PostgreSQL forbids DROP/CREATE DATABASE in a txn block.
        self._run_psql(
            f"SELECT pg_terminate_backend(pid) FROM pg_stat_activity "
            f"WHERE datname = {db_lit} AND pid <> pg_backend_pid();",
            db_url=self.admin_db_url,
        )
        self._run_psql(
            f"DROP DATABASE IF EXISTS {db_id};",
            db_url=self.admin_db_url,
        )
        self._run_psql(
            f"CREATE DATABASE {db_id};",
            db_url=self.admin_db_url,
        )

    def drop_database(self) -> None:
        db_lit = sql_literal(self.db_name)
        db_id = sql_ident(self.db_name)
        self._run_psql(
            f"SELECT pg_terminate_backend(pid) FROM pg_stat_activity "
            f"WHERE datname = {db_lit} AND pid <> pg_backend_pid();",
            db_url=self.admin_db_url,
        )
        self._run_psql(
            f"DROP DATABASE IF EXISTS {db_id};",
            db_url=self.admin_db_url,
        )

    def query_json(self, sql: str) -> Any:
        out = self._run_psql(sql, tuples_only=True)
        if not out:
            return None
        return json.loads(out)

    def query_scalar(self, sql: str) -> str:
        return self._run_psql(sql, tuples_only=True)

    def execute(self, sql: str) -> None:
        self._run_psql(sql)

    def table_exists(self, table_name: str) -> bool:
        out = self.query_scalar(
            "SELECT EXISTS (SELECT 1 FROM information_schema.tables "
            f"WHERE table_schema = 'public' AND table_name = {sql_literal(table_name)});"
        )
        return out == "t"

    def truncate_runtime_tables(self, preserve_service_clients: bool = True) -> None:
        tables = [
            "grade_posting_events",
            "sentinel_class_state",
            "auxiliary_snapshots",
            "user_sessions",
            "subject_final_results",
            "grades",
            "bimesters",
            "subjects",
            "accounts_school_years",
            "school_years",
            "accounts",
        ]
        if not preserve_service_clients:
            tables.append("service_clients")
        existing = [t for t in tables if self.table_exists(t)]
        if not existing:
            return
        joined = ", ".join(sql_ident(t) for t in existing)
        self.execute(f"TRUNCATE TABLE {joined} RESTART IDENTITY CASCADE;")

    def seed_service_client(
        self,
        name: str,
        raw_key: str,
        scopes: list[str] | None = None,
    ) -> str:
        key_hash = hashlib.sha256(raw_key.encode("utf-8")).hexdigest()
        scopes_list = scopes or ["nsac:read", "nsac:write", "account:link"]
        scopes_sql = "ARRAY[" + ", ".join(sql_literal(s) for s in scopes_list) + "]::TEXT[]"
        self.execute(
            f"""
            INSERT INTO service_clients (name, key_hash, scopes, active)
            VALUES ({sql_literal(name)}, {sql_literal(key_hash)}, {scopes_sql}, TRUE)
            ON CONFLICT (name) DO UPDATE
              SET key_hash = EXCLUDED.key_hash,
                  scopes = EXCLUDED.scopes,
                  active = TRUE;
            """
        )
        return key_hash

    def set_service_client_active(self, name: str, active: bool) -> None:
        flag = "TRUE" if active else "FALSE"
        self.execute(
            f"UPDATE service_clients SET active = {flag} WHERE name = {sql_literal(name)};"
        )

    def seed_grade_posting_events(
        self,
        class_code: str,
        *,
        offset_days: int = -2,
        years_back: int = 1,
        bimester: int = 3,
        subject_name: str = "Matemática",
        count: int = 1,
        explicit_date: date | None = None,
    ) -> list[date]:
        today_utc = datetime.now(timezone.utc).date()
        base_date = explicit_date or (today_utc + timedelta(days=offset_days))
        inserted: list[date] = []
        for idx in range(count):
            target = base_date + timedelta(days=idx if count > 1 else 0)
            target_year = target.year - years_back
            try:
                hist_date = target.replace(year=target_year)
            except ValueError:
                # Leap-day Feb 29 fallback
                hist_date = target.replace(year=target_year, day=28)
            day_of_year = hist_date.timetuple().tm_yday
            posted_at = datetime(
                hist_date.year,
                hist_date.month,
                hist_date.day,
                14,
                0,
                0,
                tzinfo=timezone.utc,
            ).isoformat()
            self.execute(
                f"""
                INSERT INTO grade_posting_events
                  (class_code, school_year, bimester, subject_name, day_of_year, month, day, posted_at)
                VALUES
                  ({sql_literal(class_code)}, {target_year}, {bimester},
                   {sql_literal(subject_name)}, {day_of_year}, {hist_date.month},
                   {hist_date.day}, {sql_literal(posted_at)}::timestamptz);
                """
            )
            inserted.append(hist_date)
        return inserted

    def expire_user_session(self, raw_token: str) -> None:
        token_hash = hashlib.sha256(raw_token.encode("utf-8")).hexdigest()
        self.execute(
            "UPDATE user_sessions SET expires_at = now() - interval '1 hour' "
            f"WHERE token_hash = {sql_literal(token_hash)};"
        )

    def set_account_active(self, email: str, active: bool) -> None:
        flag = "TRUE" if active else "FALSE"
        self.execute(
            f"UPDATE accounts SET active = {flag} WHERE nsac_email = {sql_literal(email)};"
        )

    def trigger_sentinel_due_now(self, class_code: str | None = None) -> None:
        if not self.table_exists("sentinel_class_state"):
            return
        if class_code is None:
            self.execute(
                "UPDATE sentinel_class_state SET next_check_at = now() - interval '1 second';"
            )
        else:
            self.execute(
                "UPDATE sentinel_class_state SET next_check_at = now() - interval '1 second' "
                f"WHERE class_code = {sql_literal(class_code)};"
            )

    # ---- Read-only inspection queries ----

    def get_account_row(self, email: str) -> dict[str, Any] | None:
        return self.query_json(
            "SELECT row_to_json(a) FROM ("
            "SELECT id, nsac_email, nsac_pass_enc, nsac_cookies_enc, class_code, "
            "fcm_token, discord_id, active "
            f"FROM accounts WHERE nsac_email = {sql_literal(email)} LIMIT 1"
            ") a;"
        )

    def get_account_by_id(self, account_id: int) -> dict[str, Any] | None:
        return self.query_json(
            "SELECT row_to_json(a) FROM ("
            "SELECT id, nsac_email, nsac_pass_enc, nsac_cookies_enc, class_code, "
            "fcm_token, discord_id, active "
            f"FROM accounts WHERE id = {int(account_id)} LIMIT 1"
            ") a;"
        )

    def count_accounts(self, email: str | None = None) -> int:
        if email is None:
            out = self.query_scalar("SELECT COUNT(*) FROM accounts;")
        else:
            out = self.query_scalar(
                f"SELECT COUNT(*) FROM accounts WHERE nsac_email = {sql_literal(email)};"
            )
        return int(out or "0")

    def get_service_client_row(self, name: str) -> dict[str, Any] | None:
        return self.query_json(
            "SELECT row_to_json(s) FROM ("
            "SELECT id, name, key_hash, scopes, active "
            f"FROM service_clients WHERE name = {sql_literal(name)} LIMIT 1"
            ") s;"
        )

    def get_user_sessions(self, account_id: int) -> list[dict[str, Any]]:
        res = self.query_json(
            "SELECT COALESCE(json_agg(s ORDER BY s.id), '[]'::json) FROM ("
            "SELECT id, account_id, token_hash, expires_at, revoked_at, created_at "
            f"FROM user_sessions WHERE account_id = {int(account_id)}"
            ") s;"
        )
        return list(res or [])

    def get_sentinel_state(self, class_code: str) -> dict[str, Any] | None:
        return self.query_json(
            "SELECT row_to_json(s) FROM ("
            "SELECT class_code, last_class_hash, last_checked_at, last_change_detected_at, "
            "hot_until, next_check_at, current_certainty, current_interval_secs, "
            "consecutive_errors, last_api_bump_at "
            f"FROM sentinel_class_state WHERE class_code = {sql_literal(class_code)} LIMIT 1"
            ") s;"
        )

    def get_grade_posting_events(
        self, class_code: str | None = None
    ) -> list[dict[str, Any]]:
        where = (
            f"WHERE class_code = {sql_literal(class_code)}"
            if class_code is not None
            else ""
        )
        res = self.query_json(
            "SELECT COALESCE(json_agg(e ORDER BY e.id), '[]'::json) FROM ("
            "SELECT id, class_code, school_year, bimester, subject_name, "
            "day_of_year, month, day, posted_at "
            f"FROM grade_posting_events {where}"
            ") e;"
        )
        return list(res or [])

    def count_boletim_tables_for_account(self, account_id: int) -> dict[str, int]:
        aid = int(account_id)
        return {
            "accounts_school_years": int(
                self.query_scalar(
                    f"SELECT COUNT(*) FROM accounts_school_years WHERE account_id = {aid};"
                )
            ),
            "bimesters": int(
                self.query_scalar(
                    f"SELECT COUNT(*) FROM bimesters WHERE account_id = {aid};"
                )
            ),
            "grades": int(
                self.query_scalar(
                    f"SELECT COUNT(*) FROM grades WHERE account_id = {aid};"
                )
            ),
            "subject_final_results": int(
                self.query_scalar(
                    f"SELECT COUNT(*) FROM subject_final_results WHERE account_id = {aid};"
                )
            ),
        }
