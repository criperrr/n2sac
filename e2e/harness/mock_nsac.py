"""In-process Mock NSAC Online HTTP Server reproducing live CTI DOMs & behaviors."""

from __future__ import annotations

import copy
from dataclasses import dataclass, field
import html
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import secrets
import threading
import time
from typing import Literal
from urllib.parse import parse_qs, urlparse


CTI_CLASSES: tuple[str, ...] = (
    # Diurno Integrado Informatica (6)
    "INI1A", "INI1B", "INI2A", "INI2B", "INI3A", "INI3B",
    # Diurno Integrado Eletronica (3)
    "ELI1", "ELI2", "ELI3",
    # Diurno Integrado Mecanica (3)
    "MEI1", "MEI2", "MEI3",
    # Noturno Informatica (3)
    "INF1", "INF2", "INF3",
    # Noturno Eletronica (3)
    "ELE1", "ELE2", "ELE3",
    # Noturno Mecanica (3)
    "MEC1", "MEC2", "MEC3",
)

DEFAULT_SUBJECTS: tuple[tuple[str, str], ...] = (
    ("Mat", "Matemática"),
    ("Fís", "Física"),
    ("L.Port", "Língua Portuguesa"),
    ("Quím", "Química"),
    ("Hist", "História"),
    ("POO", "Programação Orientada a Objetos"),
    ("BD", "Banco de Dados"),
)

CookieOrderMode = Literal["normal", "reversed", "remember_middle", "alternating"]
MaintenanceMode = Literal["off", "503", "delay"]


@dataclass
class MockBimesterCell:
    bimester: int
    mb: str | None
    mt: str | None
    absences: str
    rec_code: str = "-"
    rec_title: str = "Não Aconteceu"


@dataclass
class MockSubjectRow:
    abbrev: str
    full_name: str
    bimesters: list[MockBimesterCell]
    final_grade: str = "-"
    total_absences: str = ""


@dataclass
class MockYearBox:
    class_code: str
    year: int
    status: str | None
    subjects: list[MockSubjectRow]

    def compute_bimester_header_metrics(self, bimester_idx: int) -> tuple[str, int]:
        mb_values: list[float] = []
        total_absences = 0
        for subject in self.subjects:
            if bimester_idx >= len(subject.bimesters):
                continue
            cell = subject.bimesters[bimester_idx]
            if cell.mb is not None:
                raw = cell.mb.strip().replace(",", ".")
                if raw not in ("", "-", "--"):
                    try:
                        mb_values.append(float(raw))
                    except ValueError:
                        pass
            abs_raw = cell.absences.strip()
            if abs_raw not in ("", "-", "--"):
                try:
                    total_absences += int(abs_raw)
                except ValueError:
                    pass
        avg = sum(mb_values) / len(mb_values) if mb_values else 0.0
        return f"{avg:.1f}", total_absences


@dataclass
class MockAtrasoRecord:
    date: str
    time: str
    reason: str


@dataclass
class MockSaidaRecord:
    date: str
    time: str
    reason: str


@dataclass
class MockFichaEntry:
    bimester: int
    last_viewed: str = "-"
    available: bool = False
    status_text: str = "Não disponível ainda"
    detail_rows: list[tuple[str, str, str]] = field(default_factory=list)
    detail_notes: list[str] = field(default_factory=list)


@dataclass
class MockStudent:
    email: str
    password: str
    class_code: str
    years: list[MockYearBox]
    atrasos: list[MockAtrasoRecord] = field(default_factory=list)
    saidas: list[MockSaidaRecord] = field(default_factory=list)
    fichas: dict[int, MockFichaEntry] = field(default_factory=dict)
    omit_remember_cookie: bool = False
    cookie_order_override: CookieOrderMode | None = None


@dataclass
class MockNsacRequestLog:
    timestamp: float
    method: str
    path: str
    status: int
    student_email: str | None
    class_code: str | None


@dataclass
class _SessionRecord:
    email: str
    xsrf: str
    sess: str
    remember: str | None
    valid: bool = True


def _default_fichas() -> dict[int, MockFichaEntry]:
    return {
        1: MockFichaEntry(
            bimester=1,
            last_viewed="-",
            available=False,
            status_text="Você não possui ficha de avaliação! Parabéns!",
        ),
        2: MockFichaEntry(
            bimester=2,
            last_viewed="18/11/2026 14:30",
            available=True,
            status_text="Visualizar",
            detail_rows=[
                ("Matemática", "Satisfatório", "Desempenho regular nas listas."),
                ("Física", "Em recuperação", "Reforçar exercícios de cinemática."),
            ],
            detail_notes=[
                "Comparecer ao plantão de dúvidas às quartas-feiras.",
            ],
        ),
        3: MockFichaEntry(
            bimester=3,
            last_viewed="-",
            available=False,
            status_text="Não disponível ainda",
        ),
    }


def _class_mt_offset(class_code: str) -> float:
    acc = sum(ord(ch) for ch in class_code) % 10
    return round(acc * 0.1, 1)


def _prior_class_code(class_code: str, years_ago: int) -> str:
    if len(class_code) >= 4 and class_code[3].isdigit():
        grade_num = max(1, int(class_code[3]) - years_ago)
        return f"{class_code[:3]}{grade_num}{class_code[4:]}"
    return class_code


def _build_noturno_subjects(
    class_code: str,
    *,
    unposted_final: bool = True,
    lowercase_nac_tooltip: bool = False,
) -> list[MockSubjectRow]:
    nac_title = "Não aconteceu" if lowercase_nac_tooltip else "Não Aconteceu"
    offset = _class_mt_offset(class_code)
    rows: list[MockSubjectRow] = []
    # Varied grades across subjects so filter tests (classAverage, grade, recovery) match selectively.
    presets = [
        # (mb1, mt1, f1, rec1, mb2, mt2, f2, rec2, mb3, mt3, f3, rec3, mf, ft)
        ("8.5", f"{min(9.5, 7.8 + offset):.1f}", "0", "-", "7.0", f"{min(9.5, 7.2 + offset):.1f}", "2", "-", "5.5", "5.9", "4", "SAT", "7.0", "6"),
        ("6.0", "8.0", "2", "SAT", "5.0", "5.8", "4", "INS", "7.5", "7.1", "0", "-", "6.2", "6"),
        ("9.0", "7.6", "0", "-", "8.5", "7.9", "0", "-", "8.0", "7.4", "2", "-", "8.5", "2"),
        ("4.5", "5.4", "6", "NC", "6.5", "6.8", "2", "-", "7.0", "6.9", "0", "-", "6.0", "8"),
        ("7.5", "7.0", "2", "-", "8.0", "7.7", "0", "-", "7.5", "7.3", "0", "-", "7.7", "2"),
        ("8.0", "7.4", "0", "-", "7.5", "7.6", "2", "-", "8.5", "8.1", "0", "-", "8.0", "2"),
        ("6.5", "6.2", "2", "-", "7.0", "6.5", "2", "-", "6.0", "6.1", "2", "SAT", "6.5", "6"),
    ]
    rec_titles = {
        "-": nac_title,
        "SAT": "Satisfatório",
        "INS": "Insatisfatório",
        "NC": "Não Compareceu",
    }
    for (abbrev, full_name), spec in zip(DEFAULT_SUBJECTS, presets):
        mb1, mt1, f1, r1, mb2, mt2, f2, r2, mb3, mt3, f3, r3, mf, ft = spec
        bims = [
            MockBimesterCell(1, mb1, mt1, f1, r1, rec_titles[r1]),
            MockBimesterCell(2, mb2, mt2, f2, r2, rec_titles[r2]),
            MockBimesterCell(3, mb3, mt3, f3, r3, rec_titles[r3]),
            MockBimesterCell(4, "0.0", "0.0", "0", "-", nac_title),
        ]
        rows.append(
            MockSubjectRow(
                abbrev=abbrev,
                full_name=full_name,
                bimesters=bims,
                final_grade="-" if unposted_final else mf,
                total_absences="" if unposted_final else ft,
            )
        )
    return rows


def build_noturno_student(
    email: str,
    password: str,
    class_code: str = "INF1",
    *,
    year: int = 2026,
    unposted_final: bool = True,
    lowercase_nac_tooltip: bool = False,
    omit_remember_cookie: bool = False,
) -> MockStudent:
    box = MockYearBox(
        class_code=class_code,
        year=year,
        status=None,
        subjects=_build_noturno_subjects(
            class_code,
            unposted_final=unposted_final,
            lowercase_nac_tooltip=lowercase_nac_tooltip,
        ),
    )
    return MockStudent(
        email=email,
        password=password,
        class_code=class_code,
        years=[box],
        atrasos=[],
        saidas=[],
        fichas=_default_fichas(),
        omit_remember_cookie=omit_remember_cookie,
    )


def build_diurno_student(
    email: str,
    password: str,
    class_code: str = "INI3A",
    *,
    current_year: int = 2026,
    lowercase_nac_tooltip: bool = False,
    include_unposted_dash_bim2: bool = False,
    omit_remember_cookie: bool = False,
) -> MockStudent:
    nac_title = "Não aconteceu" if lowercase_nac_tooltip else "Não Aconteceu"
    years: list[MockYearBox] = []
    # Year oldest -> newest: e.g. 2024 (Aprovado), 2025 (Aprovado), 2026 (Cursando)
    for offset_idx, (yr_offset, status) in enumerate([(2, "Aprovado"), (1, "Aprovado"), (0, None)]):
        yr = current_year - yr_offset
        yr_class = _prior_class_code(class_code, yr_offset)
        subjects = _build_noturno_subjects(
            yr_class,
            unposted_final=(status is None),
            lowercase_nac_tooltip=lowercase_nac_tooltip,
        )
        if status is not None:
            for subj in subjects:
                subj.bimesters[3] = MockBimesterCell(4, "7.5", "7.2", "0", "-", nac_title)
        elif include_unposted_dash_bim2:
            # PF-EXTRA-02: Bim 1 = 8.5, Bim 2 = "-" (unposted), Bim 3 = "-", Bim 4 = "0.0"
            subjects[0].bimesters[0] = MockBimesterCell(1, "8.5", "7.8", "0", "-", nac_title)
            subjects[0].bimesters[1] = MockBimesterCell(2, "-", "-", "-", "-", nac_title)
            subjects[0].bimesters[2] = MockBimesterCell(3, "-", "-", "-", "-", nac_title)
            subjects[0].bimesters[3] = MockBimesterCell(4, "0.0", "0.0", "0", "-", nac_title)
        years.append(
            MockYearBox(
                class_code=yr_class,
                year=yr,
                status=status,
                subjects=subjects,
            )
        )
    return MockStudent(
        email=email,
        password=password,
        class_code=class_code,
        years=years,
        atrasos=[],
        saidas=[],
        fichas=_default_fichas(),
        omit_remember_cookie=omit_remember_cookie,
    )


def _parse_cookie_header(raw_cookie: str) -> dict[str, str]:
    result: dict[str, str] = {}
    if not raw_cookie:
        return result
    for part in raw_cookie.split(";"):
        part = part.strip()
        if "=" not in part:
            continue
        name, value = part.split("=", 1)
        result[name.strip()] = value.strip()
    return result


class _MockNsacHTTPServer(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True


class MockNsacServer:
    """Thread-safe synthetic NSAC Online server for opaque-box E2E tests."""

    def __init__(self) -> None:
        self._lock = threading.RLock()
        self._stop_event = threading.Event()
        self._httpd: _MockNsacHTTPServer | None = None
        self._thread: threading.Thread | None = None
        self._students: dict[str, MockStudent] = {}
        self._pending_csrf: dict[tuple[str, str], str] = {}
        self._sessions: dict[str, _SessionRecord] = {}
        self._requests: list[MockNsacRequestLog] = []
        self._cookie_order: CookieOrderMode = "normal"
        self._alternating_flip: bool = False
        self._maintenance_mode: MaintenanceMode = "off"
        self._maintenance_delay_secs: float = 20.0
        self._maintenance_path_prefix: str | None = None

    @property
    def port(self) -> int:
        if self._httpd is None:
            raise RuntimeError("MockNsacServer is not started")
        return int(self._httpd.server_address[1])

    @property
    def origin(self) -> str:
        return f"http://127.0.0.1:{self.port}"

    @property
    def base_url(self) -> str:
        return f"{self.origin}/nsac"

    def start(self) -> "MockNsacServer":
        if self._httpd is not None:
            return self
        self._stop_event.clear()
        owner = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, format: str, *args: object) -> None:  # noqa: A002
                return

            def do_GET(self) -> None:  # noqa: N802
                owner._handle_request(self, "GET")

            def do_POST(self) -> None:  # noqa: N802
                owner._handle_request(self, "POST")

        self._httpd = _MockNsacHTTPServer(("127.0.0.1", 0), Handler)
        self._thread = threading.Thread(target=self._httpd.serve_forever, daemon=True)
        self._thread.start()
        return self

    def stop(self) -> None:
        self._stop_event.set()
        if self._httpd is not None:
            self._httpd.shutdown()
            self._httpd.server_close()
            self._httpd = None
        if self._thread is not None:
            self._thread.join(timeout=3.0)
            self._thread = None

    def __enter__(self) -> "MockNsacServer":
        return self.start()

    def __exit__(self, *_: object) -> None:
        self.stop()

    def reset(self) -> None:
        with self._lock:
            self._students.clear()
            self._pending_csrf.clear()
            self._sessions.clear()
            self._requests.clear()
            self._cookie_order = "normal"
            self._alternating_flip = False
            self._maintenance_mode = "off"
            self._maintenance_delay_secs = 20.0
            self._maintenance_path_prefix = None

    # ---- Student & class management ----

    def add_student(self, student: MockStudent) -> MockStudent:
        with self._lock:
            self._students[student.email] = student
            return student

    def create_noturno_student(
        self,
        email: str,
        password: str,
        class_code: str = "INF1",
        *,
        year: int = 2026,
        unposted_final: bool = True,
        lowercase_nac_tooltip: bool = False,
        omit_remember_cookie: bool = False,
    ) -> MockStudent:
        student = build_noturno_student(
            email=email,
            password=password,
            class_code=class_code,
            year=year,
            unposted_final=unposted_final,
            lowercase_nac_tooltip=lowercase_nac_tooltip,
            omit_remember_cookie=omit_remember_cookie,
        )
        return self.add_student(student)

    def create_diurno_student(
        self,
        email: str,
        password: str,
        class_code: str = "INI3A",
        *,
        current_year: int = 2026,
        lowercase_nac_tooltip: bool = False,
        include_unposted_dash_bim2: bool = False,
        omit_remember_cookie: bool = False,
    ) -> MockStudent:
        student = build_diurno_student(
            email=email,
            password=password,
            class_code=class_code,
            current_year=current_year,
            lowercase_nac_tooltip=lowercase_nac_tooltip,
            include_unposted_dash_bim2=include_unposted_dash_bim2,
            omit_remember_cookie=omit_remember_cookie,
        )
        return self.add_student(student)

    def seed_cti_classes(
        self,
        password_prefix: str = "PassTest_",
        email_domain: str = "nsac-e2e.invalid",
    ) -> dict[str, tuple[str, str]]:
        """Create 1 synthetic student in every one of CTI's 21 classes."""
        seeded: dict[str, tuple[str, str]] = {}
        with self._lock:
            for class_code in CTI_CLASSES:
                email = f"sentinel_{class_code.lower()}@{email_domain}"
                password = f"{password_prefix}{class_code}_01!"
                if class_code.startswith(("INI", "ELI", "MEI")):
                    self.create_diurno_student(email, password, class_code=class_code)
                else:
                    self.create_noturno_student(email, password, class_code=class_code)
                seeded[class_code] = (email, password)
        return seeded

    def release_class_grade(
        self,
        class_code: str,
        subject_name: str,
        bimester: int,
        mt_grade: str,
        mb_grade: str | None = None,
        student_mb_overrides: dict[str, str] | None = None,
        rec_code: str | None = None,
        rec_title: str | None = None,
        year: int = 2026,
    ) -> int:
        updated_students = 0
        with self._lock:
            for email, student in self._students.items():
                if student.class_code != class_code:
                    continue
                for box in student.years:
                    if box.year != year:
                        continue
                    for subj in box.subjects:
                        if subj.full_name != subject_name and subj.abbrev != subject_name:
                            continue
                        if not (1 <= bimester <= len(subj.bimesters)):
                            continue
                        cell = subj.bimesters[bimester - 1]
                        cell.mt = mt_grade
                        if student_mb_overrides and email in student_mb_overrides:
                            cell.mb = student_mb_overrides[email]
                        elif mb_grade is not None:
                            cell.mb = mb_grade
                        if rec_code is not None:
                            cell.rec_code = rec_code
                        if rec_title is not None:
                            cell.rec_title = rec_title
                        updated_students += 1
        return updated_students

    def mutate_student_grade(
        self,
        email: str,
        year: int,
        subject_name: str,
        bimester: int,
        *,
        mb: str | None = None,
        mt: str | None = None,
        absences: str | None = None,
        rec_code: str | None = None,
        rec_title: str | None = None,
        final_grade: str | None = None,
        total_absences: str | None = None,
    ) -> None:
        with self._lock:
            student = self._students[email]
            for box in student.years:
                if box.year != year:
                    continue
                for subj in box.subjects:
                    if subj.full_name != subject_name and subj.abbrev != subject_name:
                        continue
                    cell = subj.bimesters[bimester - 1]
                    if mb is not None:
                        cell.mb = mb
                    if mt is not None:
                        cell.mt = mt
                    if absences is not None:
                        cell.absences = absences
                    if rec_code is not None:
                        cell.rec_code = rec_code
                    if rec_title is not None:
                        cell.rec_title = rec_title
                    if final_grade is not None:
                        subj.final_grade = final_grade
                    if total_absences is not None:
                        subj.total_absences = total_absences

    def set_student_password(self, email: str, new_password: str) -> None:
        with self._lock:
            self._students[email].password = new_password

    def expire_student_sessions(self, email: str) -> int:
        count = 0
        with self._lock:
            for record in self._sessions.values():
                if record.email == email and record.valid:
                    record.valid = False
                    count += 1
        return count

    def set_cookie_order(self, order: CookieOrderMode) -> None:
        with self._lock:
            self._cookie_order = order

    def set_omit_remember(self, email: str, omit: bool) -> None:
        with self._lock:
            self._students[email].omit_remember_cookie = omit

    def set_maintenance(
        self,
        mode: MaintenanceMode,
        delay_secs: float = 20.0,
        path_prefix: str | None = None,
    ) -> None:
        with self._lock:
            self._maintenance_mode = mode
            self._maintenance_delay_secs = delay_secs
            self._maintenance_path_prefix = path_prefix

    def set_atrasos(self, email: str, records: list[MockAtrasoRecord]) -> None:
        with self._lock:
            self._students[email].atrasos = list(records)

    def set_saidas(self, email: str, records: list[MockSaidaRecord]) -> None:
        with self._lock:
            self._students[email].saidas = list(records)

    def set_fichas(self, email: str, entries: dict[int, MockFichaEntry]) -> None:
        with self._lock:
            self._students[email].fichas = dict(entries)

    # ---- Telemetry ----

    def get_requests(
        self,
        path: str | None = None,
        student_email: str | None = None,
        class_code: str | None = None,
    ) -> list[MockNsacRequestLog]:
        with self._lock:
            items = []
            for entry in self._requests:
                if path is not None and entry.path != path:
                    continue
                if student_email is not None and entry.student_email != student_email:
                    continue
                if class_code is not None and entry.class_code != class_code:
                    continue
                items.append(copy.copy(entry))
            return items

    def count_boletim_requests(
        self,
        student_email: str | None = None,
        class_code: str | None = None,
    ) -> int:
        return len(
            self.get_requests(
                path="/nsac/aluno/boletim",
                student_email=student_email,
                class_code=class_code,
            )
        )

    def clear_requests(self) -> None:
        with self._lock:
            self._requests.clear()

    # ---- Internal HTTP dispatch ----

    def _next_order_mode(self, override: CookieOrderMode | None = None) -> CookieOrderMode:
        mode = override or self._cookie_order
        if mode == "alternating":
            self._alternating_flip = not self._alternating_flip
            return "reversed" if self._alternating_flip else "normal"
        return mode

    def _build_set_cookies(
        self,
        xsrf: str,
        sess: str,
        remember: str | None = None,
        order_override: CookieOrderMode | None = None,
    ) -> list[str]:
        by_name: dict[str, str] = {
            "XSRF-TOKEN": f"XSRF-TOKEN={xsrf}; Max-Age=7200; Path=/",
            "nsaconline_session": (
                f"nsaconline_session={sess}; Max-Age=7200; Path=/; HttpOnly"
            ),
        }
        if remember is not None:
            by_name["remember_web_synthetic_guard_01"] = (
                f"remember_web_synthetic_guard_01={remember}; "
                "Max-Age=2628000; Path=/; HttpOnly"
            )
        mode = self._next_order_mode(order_override)
        if mode == "reversed":
            names = ["remember_web_synthetic_guard_01", "nsaconline_session", "XSRF-TOKEN"]
        elif mode == "remember_middle":
            names = ["nsaconline_session", "remember_web_synthetic_guard_01", "XSRF-TOKEN"]
        else:
            names = ["XSRF-TOKEN", "nsaconline_session", "remember_web_synthetic_guard_01"]
        return [by_name[n] for n in names if n in by_name]

    def _issue_unauth_cookies(self) -> tuple[str, str, str]:
        xsrf = f"SYNTHETIC_XSRF_{secrets.token_hex(12)}"
        sess = f"SYNTHETIC_SESS_{secrets.token_hex(12)}"
        csrf = f"SYNTHETIC_CSRF_{secrets.token_hex(13)}"[:40]
        self._pending_csrf[(xsrf, sess)] = csrf
        return xsrf, sess, csrf

    def _resolve_session(self, cookie_map: dict[str, str]) -> _SessionRecord | None:
        sess_val = cookie_map.get("nsaconline_session")
        if not sess_val:
            return None
        record = self._sessions.get(sess_val)
        if record is None or not record.valid:
            return None
        if record.email not in self._students:
            return None
        return record

    def _rotate_session(self, record: _SessionRecord) -> list[str]:
        new_xsrf = f"SYNTHETIC_XSRF_{secrets.token_hex(12)}"
        new_sess = f"SYNTHETIC_SESS_{secrets.token_hex(12)}"
        record.xsrf = new_xsrf
        record.sess = new_sess
        self._sessions[new_sess] = record
        return self._build_set_cookies(
            new_xsrf,
            new_sess,
            remember=None,
            order_override=self._students[record.email].cookie_order_override,
        )

    def _record_log(
        self,
        method: str,
        path: str,
        status: int,
        student_email: str | None,
        class_code: str | None,
    ) -> None:
        self._requests.append(
            MockNsacRequestLog(
                timestamp=time.time(),
                method=method,
                path=path,
                status=status,
                student_email=student_email,
                class_code=class_code,
            )
        )

    def _send(
        self,
        handler: BaseHTTPRequestHandler,
        status: int,
        body: bytes = b"",
        *,
        content_type: str = "text/html; charset=utf-8",
        location: str | None = None,
        set_cookies: list[str] | None = None,
    ) -> None:
        handler.send_response(status)
        if location is not None:
            handler.send_header("Location", location)
        if set_cookies:
            for cookie_header in set_cookies:
                handler.send_header("Set-Cookie", cookie_header)
        if body:
            handler.send_header("Content-Type", content_type)
        handler.send_header("Content-Length", str(len(body)))
        handler.end_headers()
        if body:
            handler.wfile.write(body)

    def _handle_request(self, handler: BaseHTTPRequestHandler, method: str) -> None:
        parsed = urlparse(handler.path)
        path = parsed.path
        raw_cookie = handler.headers.get("Cookie", "")
        cookie_map = _parse_cookie_header(raw_cookie)

        with self._lock:
            maint_mode = self._maintenance_mode
            maint_delay = self._maintenance_delay_secs
            maint_prefix = self._maintenance_path_prefix
            active_session = self._resolve_session(cookie_map)
            student_email = active_session.email if active_session else None
            class_code = (
                self._students[student_email].class_code
                if student_email and student_email in self._students
                else None
            )

        if maint_mode != "off" and (
            maint_prefix is None or path.startswith(maint_prefix)
        ):
            if maint_mode == "delay":
                self._stop_event.wait(maint_delay)
            body = b"<html><body><h1>503 Manutencao Programada</h1></body></html>"
            with self._lock:
                self._record_log(method, path, 503, student_email, class_code)
            self._send(handler, 503, body)
            return

        if method == "GET" and path == "/nsac":
            with self._lock:
                self._record_log(method, path, 301, student_email, class_code)
            self._send(handler, 301, b"", location=f"{self.origin}/nsac/")
            return

        if method == "GET" and path in ("/nsac/", "/nsac/login"):
            with self._lock:
                xsrf, sess, csrf = self._issue_unauth_cookies()
                cookies = self._build_set_cookies(xsrf, sess)
                self._record_log(method, path, 200, student_email, class_code)
            body = self._render_login_html(csrf).encode("utf-8")
            self._send(handler, 200, body, set_cookies=cookies)
            return

        if method == "POST" and path == "/nsac/login":
            length = int(handler.headers.get("Content-Length", "0") or "0")
            raw_body = handler.rfile.read(length).decode("utf-8", errors="replace")
            form = parse_qs(raw_body, keep_blank_values=True)
            token = (form.get("_token") or [""])[0]
            email = (form.get("email") or [""])[0]
            password = (form.get("password") or [""])[0]
            remember_flag = (form.get("remember") or [""])[0]

            req_xsrf = cookie_map.get("XSRF-TOKEN", "")
            req_sess = cookie_map.get("nsaconline_session", "")

            with self._lock:
                expected_csrf = self._pending_csrf.pop((req_xsrf, req_sess), None)
                student = self._students.get(email)
                is_valid = (
                    expected_csrf is not None
                    and token == expected_csrf
                    and student is not None
                    and student.password == password
                )
                if is_valid and student is not None:
                    new_xsrf = f"SYNTHETIC_XSRF_{secrets.token_hex(12)}"
                    new_sess = f"SYNTHETIC_SESS_{secrets.token_hex(12)}"
                    want_rem = remember_flag.lower() in ("on", "true", "1")
                    new_rem = (
                        f"SYNTHETIC_REM_{secrets.token_hex(12)}"
                        if want_rem and not student.omit_remember_cookie
                        else None
                    )
                    self._sessions[new_sess] = _SessionRecord(
                        email=email,
                        xsrf=new_xsrf,
                        sess=new_sess,
                        remember=new_rem,
                        valid=True,
                    )
                    cookies = self._build_set_cookies(
                        new_xsrf,
                        new_sess,
                        remember=new_rem,
                        order_override=student.cookie_order_override,
                    )
                    self._record_log(method, path, 302, email, student.class_code)
                    location = f"{self.origin}/nsac/home"
                else:
                    xsrf, sess, _ = self._issue_unauth_cookies()
                    cookies = self._build_set_cookies(xsrf, sess)
                    self._record_log(
                        method,
                        path,
                        302,
                        email if student else None,
                        student.class_code if student else None,
                    )
                    location = f"{self.origin}/nsac/login"
            self._send(handler, 302, b"", location=location, set_cookies=cookies)
            return

        if method == "POST" and path == "/nsac/logout":
            with self._lock:
                if active_session is not None:
                    active_session.valid = False
                xsrf, sess, _ = self._issue_unauth_cookies()
                cookies = self._build_set_cookies(xsrf, sess)
                self._record_log(method, path, 302, student_email, class_code)
            self._send(
                handler,
                302,
                b"",
                location=f"{self.origin}/nsac/",
                set_cookies=cookies,
            )
            return

        # Protected student routes below
        if method == "GET" and path in (
            "/nsac/home",
            "/nsac/aluno/boletim",
            "/nsac/aluno/atrasos",
            "/nsac/aluno/saidas",
            "/nsac/aluno/ficha-avaliacao",
            "/nsac/change_password",
        ) or (method == "GET" and path.startswith("/nsac/aluno/ficha-avaliacao/")):
            with self._lock:
                if active_session is None:
                    xsrf, sess, _ = self._issue_unauth_cookies()
                    cookies = self._build_set_cookies(xsrf, sess)
                    self._record_log(method, path, 302, None, None)
                    self._send(
                        handler,
                        302,
                        b"",
                        location=f"{self.origin}/nsac/login",
                        set_cookies=cookies,
                    )
                    return

                student = self._students[active_session.email]
                rotated = self._rotate_session(active_session)

                if path == "/nsac/home":
                    body = self._render_home_html().encode("utf-8")
                    self._record_log(method, path, 200, student.email, student.class_code)
                    self._send(handler, 200, body, set_cookies=rotated)
                    return

                if path == "/nsac/aluno/boletim":
                    body = self._render_boletim_html(student).encode("utf-8")
                    self._record_log(method, path, 200, student.email, student.class_code)
                    self._send(handler, 200, body, set_cookies=rotated)
                    return

                if path == "/nsac/aluno/atrasos":
                    body = self._render_atrasos_html(student).encode("utf-8")
                    self._record_log(method, path, 200, student.email, student.class_code)
                    self._send(handler, 200, body, set_cookies=rotated)
                    return

                if path == "/nsac/aluno/saidas":
                    body = self._render_saidas_html(student).encode("utf-8")
                    self._record_log(method, path, 200, student.email, student.class_code)
                    self._send(handler, 200, body, set_cookies=rotated)
                    return

                if path == "/nsac/aluno/ficha-avaliacao":
                    body = self._render_ficha_list_html(student).encode("utf-8")
                    self._record_log(method, path, 200, student.email, student.class_code)
                    self._send(handler, 200, body, set_cookies=rotated)
                    return

                if path == "/nsac/change_password":
                    body = self._render_change_password_html().encode("utf-8")
                    self._record_log(method, path, 200, student.email, student.class_code)
                    self._send(handler, 200, body, set_cookies=rotated)
                    return

                # /nsac/aluno/ficha-avaliacao/{id}
                raw_id = path[len("/nsac/aluno/ficha-avaliacao/"):]
                if raw_id == "0":
                    self._record_log(method, path, 302, student.email, student.class_code)
                    self._send(
                        handler,
                        302,
                        b"",
                        location=f"{self.origin}/nsac/aluno/ficha-avaliacao/1",
                        set_cookies=rotated,
                    )
                    return
                try:
                    bimester_id = int(raw_id)
                except ValueError:
                    bimester_id = -1
                entry = student.fichas.get(bimester_id)
                if entry is None or not entry.available:
                    self._record_log(method, path, 302, student.email, student.class_code)
                    self._send(
                        handler,
                        302,
                        b"",
                        location=f"{self.origin}/nsac/aluno/ficha-avaliacao",
                        set_cookies=rotated,
                    )
                    return
                year = student.years[-1].year if student.years else 2026
                body = self._render_ficha_detail_html(entry, year).encode("utf-8")
                self._record_log(method, path, 200, student.email, student.class_code)
                self._send(handler, 200, body, set_cookies=rotated)
                return

        with self._lock:
            self._record_log(method, path, 404, student_email, class_code)
        self._send(handler, 404, b"<html><body><h1>Page Not Found</h1></body></html>")

    # ---- HTML renderers matching sanitized_dom_blueprints.md ----

    def _render_login_html(self, csrf_token: str) -> str:
        origin = self.origin
        return f"""<!DOCTYPE html>
<html>
<head>
  <meta charset="utf-8">
  <title>NSac Online - NSac Online</title>
</head>
<body class="hold-transition login-page">
  <div class="login-box">
    <div class="login-box-body">
      <div class="login-logo">
        <a href="{origin}/nsac/home"><b>NSac</b> Online</a>
      </div>
      <p class="login-box-msg">Entre para iniciar uma nova sessão</p>
      <form action="{origin}/nsac/login" method="post">
        <input type="hidden" name="_token" value="{html.escape(csrf_token)}">
        <div class="form-group has-feedback">
          <input type="email" name="email" class="form-control" placeholder="Email" value="">
        </div>
        <div class="form-group has-feedback">
          <input type="password" name="password" class="form-control" placeholder="Senha">
        </div>
        <div class="row">
          <div class="col-xs-8">
            <div class="checkbox icheck">
              <label><input type="checkbox" name="remember"> Lembrar-me</label>
            </div>
          </div>
          <div class="col-xs-4">
            <button type="submit" class="btn btn-primary btn-block btn-flat">Entrar</button>
          </div>
        </div>
      </form>
      <div class="auth-links">
        <a href="{origin}/nsac/password/reset" class="text-center">Esqueci minha senha</a><br>
      </div>
    </div>
  </div>
</body>
</html>"""

    def _render_home_html(self) -> str:
        origin = self.origin
        return f"""<!DOCTYPE html>
<html>
<head>
  <meta charset="utf-8">
  <title>NSac Online - Bem-Vindo</title>
</head>
<body class="hold-transition skin-blue sidebar-mini">
<div class="wrapper">
  <header class="main-header">
    <a href="{origin}/nsac/home" class="logo">
      <span class="logo-mini"><b>N</b>O</span>
      <span class="logo-lg"><b>NSac</b> Online</span>
    </a>
    <nav class="navbar navbar-static-top" role="navigation">
      <div class="navbar-custom-menu">
        <ul class="nav navbar-nav">
          <li>
            <a href="#"><i class="fa fa-fw fa-power-off"></i> Sair</a>
            <form id="logout-form" action="{origin}/nsac/logout" method="POST" style="display: none;">
              <input type="hidden" name="_token" value="SYNTHETIC_CSRF_TOKEN">
            </form>
          </li>
        </ul>
      </div>
    </nav>
  </header>
  <aside class="main-sidebar">
    <section class="sidebar">
      <ul class="sidebar-menu" data-widget="tree">
        <li class="header">MENU PRINCIPAL</li>
        <li class="header">ALUNO</li>
        <li class=""><a href="{origin}/nsac/aluno/boletim">Boletim</a></li>
        <li class=""><a href="{origin}/nsac/aluno/atrasos">Atrasos</a></li>
        <li class=""><a href="{origin}/nsac/aluno/saidas">Saídas</a></li>
        <li class=""><a href="{origin}/nsac/aluno/ficha-avaliacao">Fichas de Avaliação</a></li>
        <li class="header">GERAL</li>
        <li class=""><a href="{origin}/nsac/change_password">Alterar Senha</a></li>
      </ul>
    </section>
  </aside>
  <div class="content-wrapper">
    <section class="content-header">
      <h1>NSac Online <small>19/11/2026</small></h1>
    </section>
    <section class="content">
      <div class="container-fluid">
        <div class="row">
          <div class="col-md-12 col-lg-4">
            <div class="info-box">
              <div class="info-box-content">
                <span class="info-box-text">Facebook</span>
                <span class="info-box-number">Curta a página oficial do CTI no Facebook</span>
              </div>
            </div>
          </div>
          <div class="col-md-12 col-lg-4">
            <div class="info-box">
              <div class="info-box-content">
                <span class="info-box-text">Site do CTI</span>
                <span class="info-box-number">Acesse o site oficial do colégio</span>
              </div>
            </div>
          </div>
          <div class="col-md-12 col-lg-4">
            <div class="info-box">
              <div class="info-box-content">
                <span class="info-box-text">Site da UNESP</span>
                <span class="info-box-number">Portal da Universidade Estadual Paulista</span>
              </div>
            </div>
          </div>
        </div>
      </div>
    </section>
  </div>
</div>
</body>
</html>"""

    def _render_grade_cell(self, value: str | None, *, is_mt: bool) -> str:
        text = "-" if value is None else value
        numeric_low = False
        try:
            numeric_low = float(text.replace(",", ".")) < 6.0
        except ValueError:
            numeric_low = False
        if is_mt:
            cls = "text-center text-red" if numeric_low else "text-center"
            return f'<td class="{cls}">{html.escape(text)}</td>'
        if numeric_low:
            return f'<td class="text-center"><span style="color:red;">{html.escape(text)}</span></td>'
        if text not in ("", "-", "--"):
            return f'<td class="text-center"><span>{html.escape(text)}</span></td>'
        return f'<td class="text-center">{html.escape(text)}</td>'

    def _render_boletim_html(self, student: MockStudent) -> str:
        boxes_html: list[str] = []
        # Render newest-to-oldest in DOM just like live NSAC.
        for box in reversed(student.years):
            header_text = (
                f"{box.class_code}/{box.year}"
                if box.status is None
                else f"{box.class_code}/{box.year} - {box.status}"
            )
            bim_headers: list[str] = []
            for b_idx in range(4):
                avg_str, faltas_sum = box.compute_bimester_header_metrics(b_idx)
                bim_headers.append(
                    '<th class="text-center bi1 bd" colspan="4" '
                    'style="border-right: 1px solid #e7e7e7">\n'
                    f"                {b_idx + 1}º BIMESTRE<br>\n"
                    f"                <small>Média: {avg_str}</small> - "
                    f"<small>Faltas: {faltas_sum}</small>\n"
                    "              </th>"
                )
            rows_html: list[str] = []
            for subj in box.subjects:
                cells_html: list[str] = []
                for b_idx in range(4):
                    cell = subj.bimesters[b_idx]
                    cells_html.append(self._render_grade_cell(cell.mb, is_mt=False))
                    cells_html.append(self._render_grade_cell(cell.mt, is_mt=True))
                    cells_html.append(
                        f'<td class="text-center bd">{html.escape(cell.absences)}</td>'
                    )
                    cells_html.append(
                        '<td class="text-center bd">'
                        f'<span data-toggle="tooltip" title="{html.escape(cell.rec_title)}">'
                        f"{html.escape(cell.rec_code)}</span></td>"
                    )
                rows_html.append(
                    '<tr class="linha">\n'
                    f'              <td><span title="{html.escape(subj.full_name)}">'
                    f"{html.escape(subj.abbrev)}</span></td>\n"
                    f"              {''.join(cells_html)}\n"
                    f'              <td class="text-center">{html.escape(subj.final_grade)}</td>\n'
                    f'              <td class="text-center">{html.escape(subj.total_absences)}</td>\n'
                    "            </tr>"
                )
            boxes_html.append(
                f"""    <div class="box box-primary">
      <div class="box-header with-border">
        <h4 class="col-md-8 col-xs-12">{html.escape(header_text)}</h4>
      </div>
      <div class="box-body">
        <table id="notas" class="table table-responsive table-notas">
          <thead>
            <tr>
              <th>Disciplina</th>
              {''.join(bim_headers)}
              <th class="text-center bi1" colspan="2">RESULTADO<br></th>
            </tr>
            <tr id="contador" class="cabecalho">
              <th><span title="Nome da disciplina">Nome</span></th>
              <th class="bi1 text-center"><div data-toggle="tooltip" title="Média Bimestral">M.B.</div></th>
              <th class="bi1 text-center"><div data-toggle="tooltip" title="Média da Turma">M.T.</div></th>
              <th class="bi1 text-center bd"><div data-toggle="tooltip" title="Faltas">F.</div></th>
              <th class="text-center bd" style="border-right: 1px solid #e7e7e7">REC</th>
              <th class="bi2 text-center"><div data-toggle="tooltip" title="Média Bimestral">M.B.</div></th>
              <th class="bi2 text-center"><div data-toggle="tooltip" title="Média da Turma">M.T.</div></th>
              <th class="bi2 text-center bd"><div data-toggle="tooltip" title="Faltas">F.</div></th>
              <th class="text-center bd" style="border-right: 1px solid #e7e7e7">REC</th>
              <th class="bi3 text-center"><div data-toggle="tooltip" title="Média Bimestral">M.B.</div></th>
              <th class="bi3 text-center"><div data-toggle="tooltip" title="Média da Turma">M.T.</div></th>
              <th class="bi3 text-center bd"><div data-toggle="tooltip" title="Faltas">F.</div></th>
              <th class="text-center bd" style="border-right: 1px solid #e7e7e7">REC</th>
              <th class="bi4 text-center"><div data-toggle="tooltip" title="Média Bimestral">M.B.</div></th>
              <th class="bi4 text-center"><div data-toggle="tooltip" title="Média da Turma">M.T.</div></th>
              <th class="bi4 text-center bd"><div data-toggle="tooltip" title="Faltas">F.</div></th>
              <th class="text-center bd" style="border-right: 1px solid #e7e7e7">REC</th>
              <th class="text-center"><div data-toggle="tooltip" title="Média Final">M.F.</div></th>
              <th class="text-center"><div data-toggle="tooltip" title="Faltas totais">F.T.</div></th>
            </tr>
          </thead>
          <tbody>
            {''.join(rows_html)}
          </tbody>
        </table>
      </div>
    </div>"""
            )
        return f"""<!DOCTYPE html>
<html>
<head>
  <meta charset="utf-8">
  <title>NSac Online - Boletim</title>
</head>
<body>
<div class="content-wrapper">
  <section class="content-header">
    <h1>Boletim</h1>
  </section>
  <section class="content">
    {''.join(boxes_html)}
  </section>
</div>
</body>
</html>"""

    def _render_atrasos_html(self, student: MockStudent) -> str:
        table_block = ""
        if student.atrasos:
            rows = "".join(
                f"<tr><td>{html.escape(r.date)}</td><td>{html.escape(r.time)}</td>"
                f"<td>{html.escape(r.reason)}</td></tr>"
                for r in student.atrasos
            )
            table_block = f"""
    <div class="box box-primary">
      <div class="box-body">
        <table class="table table-responsive">
          <thead>
            <tr><th>Data</th><th>Horário</th><th>Motivo</th></tr>
          </thead>
          <tbody>{rows}</tbody>
        </table>
      </div>
    </div>"""
        return f"""<!DOCTYPE html>
<html>
<head>
  <meta charset="utf-8">
  <title>NSac Online - Atrasos</title>
</head>
<body>
<div class="content-wrapper">
  <section class="content-header">
    <h1>Atrasos <small>Lista de atrasos</small></h1>
  </section>
  <section class="content">
    <div class="callout callout-warning">
      <h4><i class="icon fa fa-warning"></i> Regras:</h4>
      <p>3º atraso no semestre: Ocorrência Disciplinar</p>
      <p>4º atraso no semestre: Não entra na aula</p>
    </div>{table_block}
  </section>
</div>
</body>
</html>"""

    def _render_saidas_html(self, student: MockStudent) -> str:
        table_block = ""
        if student.saidas:
            rows = "".join(
                f"<tr><td>{html.escape(r.date)}</td><td>{html.escape(r.time)}</td>"
                f"<td>{html.escape(r.reason)}</td></tr>"
                for r in student.saidas
            )
            table_block = f"""
    <div class="box box-primary">
      <div class="box-body">
        <table class="table table-responsive">
          <thead>
            <tr><th>Data</th><th>Horário</th><th>Motivo</th></tr>
          </thead>
          <tbody>{rows}</tbody>
        </table>
      </div>
    </div>"""
        return f"""<!DOCTYPE html>
<html>
<head>
  <meta charset="utf-8">
  <title>NSac Online - Saídas</title>
</head>
<body>
<div class="content-wrapper">
  <section class="content-header">
    <h1>Saídas <small>Lista de saídas</small></h1>
  </section>
  <section class="content">
    <div class="callout callout-warning">
      <h4><i class="icon fa fa-warning"></i> Regras:</h4>
      <p>Para maiores de 18 anos: Solicitada pelo próprio aluno na secretaria</p>
      <p>Para menores de 18 anos: Necessita autorização dos pais e/ou responsáveis</p>
    </div>{table_block}
  </section>
</div>
</body>
</html>"""

    def _render_ficha_list_html(self, student: MockStudent) -> str:
        year = student.years[-1].year if student.years else 2026
        origin = self.origin
        rows: list[str] = []
        for bim in sorted(student.fichas.keys()):
            entry = student.fichas[bim]
            if entry.available:
                opt_html = (
                    f'<a href="{origin}/nsac/aluno/ficha-avaliacao/{entry.bimester}">'
                    f"{html.escape(entry.status_text)}</a>"
                )
            else:
                opt_html = html.escape(entry.status_text)
            rows.append(
                f"<tr><td>{entry.bimester}</td>"
                f"<td>{html.escape(entry.last_viewed)}</td>"
                f"<td>{opt_html}</td></tr>"
            )
        return f"""<!DOCTYPE html>
<html>
<head>
  <meta charset="utf-8">
  <title>NSac Online - Saídas</title>
</head>
<body>
<div class="content-wrapper">
  <section class="content-header">
    <h1>Fichas de Avaliação <small>Visualização das Fichas de Avaliação</small></h1>
  </section>
  <section class="content">
    <div class="box box-default">
      <div class="box-header">
        <h3 class="box-title">Fichas de Avaliação do ano de {year}</h3>
      </div>
      <div class="box-body">
        <table class="table table-responsive">
          <thead>
            <tr>
              <th>Bimestre</th>
              <th>Última Visualização</th>
              <th>Opções</th>
            </tr>
          </thead>
          <tbody>
            {''.join(rows)}
          </tbody>
        </table>
      </div>
    </div>
  </section>
</div>
</body>
</html>"""

    def _render_ficha_detail_html(self, entry: MockFichaEntry, year: int) -> str:
        rows = "".join(
            f"<tr><td>{html.escape(d)}</td><td>{html.escape(p)}</td>"
            f"<td>{html.escape(o)}</td></tr>"
            for d, p, o in entry.detail_rows
        )
        notes = "".join(f"<p>{html.escape(note)}</p>" for note in entry.detail_notes)
        return f"""<!DOCTYPE html>
<html>
<head>
  <meta charset="utf-8">
  <title>NSac Online - Ficha de Avaliação</title>
</head>
<body>
<div class="content-wrapper">
  <section class="content-header">
    <h1>Ficha de Avaliação <small>{entry.bimester}º Bimestre/{year}</small></h1>
  </section>
  <section class="content">
    <div class="box box-default">
      <div class="box-header">
        <h3 class="box-title">Ficha de Avaliação - {entry.bimester}º Bimestre/{year}</h3>
      </div>
      <div class="box-body">
        <table class="table table-responsive">
          <thead>
            <tr><th>Disciplina</th><th>Desempenho</th><th>Observação</th></tr>
          </thead>
          <tbody>{rows}</tbody>
        </table>
        {notes}
      </div>
    </div>
  </section>
</div>
</body>
</html>"""

    def _render_change_password_html(self) -> str:
        return """<!DOCTYPE html>
<html>
<head>
  <meta charset="utf-8">
  <title>NSac Online - NSac Online</title>
</head>
<body>
<div class="content-wrapper">
  <section class="content">
    <form action="/usuario/edit/senha/store" method="post">
      <input type="hidden" name="_token" value="SYNTHETIC_CSRF_TOKEN">
      <input type="password" name="atual" placeholder="Senha atual">
      <input type="password" name="nova" placeholder="Nova senha">
      <input type="password" name="confirmacao" placeholder="Confirmação">
      <button type="submit">Alterar</button>
    </form>
  </section>
</div>
</body>
</html>"""
