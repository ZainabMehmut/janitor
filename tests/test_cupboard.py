#!/usr/bin/python
# Copyright (C) 2019 Jelmer Vernooij <jelmer@jelmer.uk>
#
# This program is free software; you can redistribute it and/or modify
# it under the terms of the GNU General Public License as published by
# the Free Software Foundation; either version 2 of the License, or
# (at your option) any later version.
#
# This program is distributed in the hope that it will be useful,
# but WITHOUT ANY WARRANTY; without even the implied warranty of
# MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
# GNU General Public License for more details.
#
# You should have received a copy of the GNU General Public License
# along with this program; if not, write to the Free Software
# Foundation, Inc., 51 Franklin Street, Fifth Floor, Boston, MA 02110-1301 USA

import asyncio
import sys
import types
from datetime import datetime, timedelta, timezone

import aiohttp_jinja2
import pytest
from aiohttp import web
from bs4 import BeautifulSoup
from fakeredis.aioredis import FakeRedis
from jinja2 import Environment
from yarl import URL

from janitor import utcnow
from janitor.config import read_string as read_config_string
from janitor.site import (
    TEMPLATE_ENV,
    classify_result_code,
    format_duration,
    format_timestamp,
    template_loader,
    worker_link_is_global,
)
from janitor.site.cupboard import create_app
from janitor.site.cupboard.api import create_app as create_api_app
from janitor.site.cupboard.api import select_runs_to_reprocess
from janitor.site.cupboard.publish import iter_publish_history
from redis.asyncio import Redis

from .conftest import _insert_codebase, _insert_run


def _user_middleware(user):
    @web.middleware
    async def middleware(request, handler):
        request["user"] = user
        return await handler(request)

    return middleware


async def create_client(aiohttp_client, db, *, user=None, config="", **app_keys):
    config = read_config_string(config)
    app = create_app(
        config=config, publisher_url=None, runner_url=None, differ_url=None, db=db
    )
    app.update(app_keys)
    app["external_url"] = URL("http://example.com/")
    # create_app leaves the jinja globals to its caller, and the production
    # caller is py/janitor/site/simple.py.
    aiohttp_jinja2.get_env(app).globals.update(TEMPLATE_ENV)
    app.middlewares.insert(0, _user_middleware(user))
    return await aiohttp_client(app)


async def create_api_client(aiohttp_client, db, *, user):
    config = read_config_string("oauth2_provider {}\n")
    app = create_api_app(config=config, publisher_url=None, runner_url=None, db=db)
    app.middlewares.insert(0, _user_middleware(user))
    return await aiohttp_client(app)


def test_render_merge_proposal():
    env = Environment(loader=template_loader)
    template = env.get_template("cupboard/merge-proposal.html")
    template.render(
        proposal={
            "url": "https://github.com/jelmer/example/pulls/1",
            "package": "zz",
        }
    )


def test_render_run():
    env = Environment(loader=template_loader)
    template = env.get_template("cupboard/run.html")
    config = read_config_string("""\
campaign {
  name: "some-fixes"
}
""")
    campaign = config.campaign[0]
    template.render(
        worker_link_is_global=worker_link_is_global,
        run={
            "start_time": utcnow(),
            "finish_time": utcnow(),
        },
        success_probability=0.2,
        classify_result_code=classify_result_code,
        show_debdiff=lambda: "",
        campaign=campaign,
        config=config,
        publish_blockers=dict,
        format_timestamp=format_timestamp,
        format_duration=format_duration,
    )


def test_classify_result_code_debian_validate():
    assert (
        classify_result_code("apt-repository-setup-failure", None)
        == "transient-failure"
    )
    assert (
        classify_result_code("vcs-up-to-date-check-failed", None) == "transient-failure"
    )
    assert classify_result_code("missing-dependency", None) == "bug"


async def test_history(aiohttp_client, db):
    client = await create_client(aiohttp_client, db)
    resp = await client.get("/cupboard/history")
    assert resp.status == 200


async def test_queue(aiohttp_client, db):
    client = await create_client(aiohttp_client, db)
    resp = await client.get("/cupboard/queue")
    assert resp.status == 200


async def test_publish_history(aiohttp_client, db):
    client = await create_client(aiohttp_client, db)
    resp = await client.get("/cupboard/publish")
    assert resp.status == 200


ADMIN_GROUP_CONFIG = 'oauth2_provider { admin_group: "admins" }\n'


@pytest.mark.parametrize("user", [None, {"email": "user@example.com", "groups": []}])
async def test_reprocess_logs_requires_admin(aiohttp_client, db, user):
    client = await create_client(
        aiohttp_client, db, user=user, config=ADMIN_GROUP_CONFIG
    )
    resp = await client.get("/cupboard/reprocess-logs")
    assert resp.status == 401


async def test_reprocess_logs_admin(aiohttp_client, db):
    client = await create_client(
        aiohttp_client,
        db,
        user={"email": "admin@example.com", "groups": ["admins"]},
        config=ADMIN_GROUP_CONFIG,
    )
    resp = await client.get("/cupboard/reprocess-logs")
    assert resp.status == 200


def test_render_changeset():
    env = Environment(loader=template_loader)
    template = env.get_template("cupboard/changeset.html")
    template.render(changeset="changeset", url=URL("https://example/com"), URL=URL)


def test_render_broken_mps():
    env = Environment(loader=template_loader)
    env.get_template("cupboard/broken-merge-proposals.html")


def test_render_changeset_list():
    env = Environment(loader=template_loader)
    env.get_template("cupboard/changeset-list.html")


def test_render_rejected():
    env = Environment(loader=template_loader)
    env.get_template("cupboard/rejected.html")


def test_workers():
    env = Environment(loader=template_loader)
    template = env.get_template("cupboard/workers.html")
    template.render(worker_link_is_global=worker_link_is_global)


def test_start():
    env = Environment(loader=template_loader)
    env.get_template("cupboard/start.html")


def test_reprocess_logs():
    env = Environment(loader=template_loader)
    env.get_template("cupboard/reprocess-logs.html")


def test_never_processed():
    env = Environment(loader=template_loader)
    env.get_template("cupboard/never-processed.html")


def test_result_code_index():
    env = Environment(loader=template_loader)
    env.get_template("cupboard/result-code-index.html")


def test_result_code():
    env = Environment(loader=template_loader)
    env.get_template("cupboard/result-code.html")


def test_failure_stage():
    env = Environment(loader=template_loader)
    env.get_template("cupboard/failure-stage-index.html")


def test_publish():
    env = Environment(loader=template_loader)
    env.get_template("cupboard/publish.html")


def test_run():
    env = Environment(loader=template_loader)
    env.get_template("cupboard/run.html")


async def test_codebase_redirect_to_latest_run(aiohttp_client, db):
    client = await create_client(aiohttp_client, db)
    async with db.acquire() as conn:
        await _insert_codebase(conn, "foo")
        now = utcnow()
        await _insert_run(
            conn,
            run_id="older",
            codebase="foo",
            start_time=now - timedelta(hours=2),
            finish_time=now - timedelta(hours=1),
        )
        await _insert_run(
            conn,
            run_id="newer",
            codebase="foo",
            start_time=now - timedelta(minutes=30),
            finish_time=now,
        )

    resp = await client.get("/cupboard/c/foo/", allow_redirects=False)
    assert resp.status == 302
    assert resp.headers["Location"] == "/cupboard/c/foo/newer/"


async def test_codebase_redirect_prefers_finished_run(aiohttp_client, db):
    # finish_time DESC NULLS LAST: a finished run wins over an in-progress one
    # (finish_time IS NULL) that started later
    client = await create_client(aiohttp_client, db)
    async with db.acquire() as conn:
        await _insert_codebase(conn, "foo")
        now = utcnow()
        await _insert_run(
            conn,
            run_id="done",
            codebase="foo",
            start_time=now - timedelta(hours=1),
            finish_time=now - timedelta(minutes=30),
        )
        await _insert_run(
            conn,
            run_id="running",
            codebase="foo",
            start_time=now,
            finish_time=None,
        )

    resp = await client.get("/cupboard/c/foo/", allow_redirects=False)
    assert resp.status == 302
    assert resp.headers["Location"] == "/cupboard/c/foo/done/"


async def test_codebase_redirect_no_runs_returns_404(aiohttp_client, db):
    client = await create_client(aiohttp_client, db)
    async with db.acquire() as conn:
        await _insert_codebase(conn, "foo")

    resp = await client.get("/cupboard/c/foo/", allow_redirects=False)
    assert resp.status == 404


async def test_codebase_redirect_unknown_codebase_returns_404(aiohttp_client, db):
    client = await create_client(aiohttp_client, db)
    resp = await client.get("/cupboard/c/nonexistent/", allow_redirects=False)
    assert resp.status == 404


async def test_run_redirect(aiohttp_client, db):
    client = await create_client(aiohttp_client, db)
    async with db.acquire() as conn:
        await _insert_codebase(conn, "foo")
        now = utcnow()
        await _insert_run(
            conn,
            run_id="somerun",
            codebase="foo",
            start_time=now - timedelta(minutes=30),
            finish_time=now,
        )

    resp = await client.get("/cupboard/run/somerun/", allow_redirects=False)
    assert resp.status == 308
    assert resp.headers["Location"] == "/cupboard/c/foo/somerun/"


async def test_run_redirect_unknown_run_returns_404(aiohttp_client, db):
    client = await create_client(aiohttp_client, db)
    resp = await client.get("/cupboard/run/nonexistent/", allow_redirects=False)
    assert resp.status == 404


async def test_evaluate_unknown_run_returns_404(db):
    from janitor.site.cupboard.review import generate_evaluate

    with pytest.raises(web.HTTPNotFound):
        await generate_evaluate(db, {}, None, None, "nonexistent", None)


ADMIN_USER = {"email": "admin@example.com", "groups": []}


async def test_workers_list_requires_admin(aiohttp_client, db):
    client = await create_api_client(aiohttp_client, db, user=None)
    resp = await client.get("/workers")
    assert resp.status == 401


async def test_workers_list_empty(aiohttp_client, db):
    client = await create_api_client(aiohttp_client, db, user=ADMIN_USER)
    resp = await client.get("/workers")
    assert resp.status == 200
    assert await resp.json() == []


async def test_workers_list(aiohttp_client, db):
    client = await create_api_client(aiohttp_client, db, user=ADMIN_USER)
    async with db.acquire() as conn:
        await conn.execute(
            "INSERT INTO worker (name, password, link) VALUES "
            "($1, 'x', $2), ($3, 'x', $4)",
            "alice",
            "https://public.example.com/",
            "bob",
            "http://10.0.0.1/",
        )
    resp = await client.get("/workers")
    assert resp.status == 200
    body = await resp.json()
    assert body == [
        {"name": "alice", "link": "https://public.example.com/", "run_count": 0},
        {"name": "bob", "link": None, "run_count": 0},
    ]


async def test_workers_list_counts_runs(aiohttp_client, db):
    client = await create_api_client(aiohttp_client, db, user=ADMIN_USER)
    async with db.acquire() as conn:
        await conn.execute(
            "INSERT INTO worker (name, password) VALUES ($1, 'x')", "alice"
        )
        await _insert_codebase(conn, "foo")
        now = utcnow()
        await _insert_run(
            conn,
            run_id="r1",
            codebase="foo",
            start_time=now - timedelta(hours=1),
            finish_time=now,
        )
        await conn.execute("UPDATE run SET worker = 'alice' WHERE id = 'r1'")

    resp = await client.get("/workers")
    assert resp.status == 200
    assert await resp.json() == [{"name": "alice", "link": None, "run_count": 1}]


async def test_workers_add_requires_admin(aiohttp_client, db):
    client = await create_api_client(aiohttp_client, db, user=None)
    resp = await client.post("/workers", data={"name": "alice"})
    assert resp.status == 401


async def test_workers_add_requires_name(aiohttp_client, db):
    client = await create_api_client(aiohttp_client, db, user=ADMIN_USER)
    resp = await client.post("/workers", data={})
    assert resp.status == 400


async def test_workers_add(aiohttp_client, db):
    client = await create_api_client(aiohttp_client, db, user=ADMIN_USER)
    resp = await client.post(
        "/workers", data={"name": "alice", "link": "https://alice.example.com/"}
    )
    assert resp.status == 201
    body = await resp.json()
    assert body["name"] == "alice"
    assert body["link"] == "https://alice.example.com/"
    assert isinstance(body["password"], str) and body["password"]

    async with db.acquire() as conn:
        row = await conn.fetchrow(
            "SELECT name, link, password FROM worker WHERE name = 'alice'"
        )
    assert row["name"] == "alice"
    assert row["link"] == "https://alice.example.com/"
    # password is stored hashed, not in plaintext
    assert row["password"] != body["password"]


async def test_workers_add_without_link(aiohttp_client, db):
    client = await create_api_client(aiohttp_client, db, user=ADMIN_USER)
    resp = await client.post("/workers", data={"name": "alice"})
    assert resp.status == 201
    body = await resp.json()
    assert body["link"] is None

    async with db.acquire() as conn:
        row = await conn.fetchrow("SELECT link FROM worker WHERE name = 'alice'")
    assert row["link"] is None


async def test_workers_add_duplicate(aiohttp_client, db):
    client = await create_api_client(aiohttp_client, db, user=ADMIN_USER)
    async with db.acquire() as conn:
        await conn.execute("INSERT INTO worker (name, password) VALUES ('alice', 'x')")
    resp = await client.post("/workers", data={"name": "alice"})
    assert resp.status == 409


async def test_workers_delete_requires_admin(aiohttp_client, db):
    client = await create_api_client(aiohttp_client, db, user=None)
    resp = await client.delete("/workers/alice")
    assert resp.status == 401


async def test_workers_delete(aiohttp_client, db):
    client = await create_api_client(aiohttp_client, db, user=ADMIN_USER)
    async with db.acquire() as conn:
        await conn.execute("INSERT INTO worker (name, password) VALUES ('alice', 'x')")
    resp = await client.delete("/workers/alice")
    assert resp.status == 200
    assert await resp.json() == {"name": "alice"}

    async with db.acquire() as conn:
        count = await conn.fetchval("SELECT count(*) FROM worker WHERE name = 'alice'")
    assert count == 0


async def test_workers_delete_unknown(aiohttp_client, db):
    client = await create_api_client(aiohttp_client, db, user=ADMIN_USER)
    resp = await client.delete("/workers/nonexistent")
    assert resp.status == 404


async def test_history_limit_zero_lists_no_runs(aiohttp_client, db):
    client = await create_client(aiohttp_client, db)
    async with db.acquire() as conn:
        await _insert_codebase(conn, "foo")
        now = utcnow()
        await _insert_run(
            conn,
            run_id="somerun",
            codebase="foo",
            start_time=now - timedelta(minutes=30),
            finish_time=now,
        )

    resp = await client.get("/cupboard/history")
    assert resp.status == 200
    assert "somerun" in await resp.text()

    resp = await client.get("/cupboard/history?limit=0")
    assert resp.status == 200
    assert "somerun" not in await resp.text()


async def test_publish_history_limit_zero_lists_nothing(con):
    await con.execute("INSERT INTO codebase (name) VALUES ('foo')")
    await con.execute(
        "INSERT INTO change_set (id, campaign) VALUES ('cs1', 'mycampaign')"
    )
    await con.execute(
        "INSERT INTO publish (id, change_set, target_branch_url, mode, "
        "result_code, codebase) VALUES "
        "('p1', 'cs1', 'https://example.com/foo.git', 'propose', 'success', 'foo'), "
        "('p2', 'cs1', 'https://example.com/foo.git', 'propose', 'success', 'foo')"
    )
    assert len(await iter_publish_history(con)) == 2
    assert len(await iter_publish_history(con, limit=1)) == 1
    assert await iter_publish_history(con, limit=0) == []


@pytest.fixture
def reprocess_logs_stub(monkeypatch):
    # The handler imports this module on each request and only hands the rows on.
    async def reprocess_run_logs(**kwargs):
        return None

    stub = types.ModuleType("janitor.reprocess_logs")
    vars(stub).update(
        reprocess_run_logs=reprocess_run_logs,
        process_dist_log=None,
        process_sbuild_log=None,
    )
    monkeypatch.setitem(sys.modules, "janitor.reprocess_logs", stub)


async def test_reprocess_logs_bulk_filters_by_campaign(
    aiohttp_client, db, reprocess_logs_stub
):
    client = await create_api_client(aiohttp_client, db, user=ADMIN_USER)
    async with db.acquire() as conn:
        await _insert_codebase(conn, "foo")
        await _insert_codebase(conn, "bar")
        now = utcnow()
        await _insert_run(
            conn,
            run_id="matching",
            codebase="foo",
            campaign="lintian-fixes",
            start_time=now - timedelta(minutes=30),
            finish_time=now,
            result_code="build-failed",
        )
        await _insert_run(
            conn,
            run_id="other-campaign",
            codebase="bar",
            campaign="multiarch-hints",
            start_time=now - timedelta(minutes=30),
            finish_time=now,
            result_code="build-failed",
        )

    resp = await client.post("/reprocess-logs", data={"campaign": "lintian-fixes"})
    assert resp.status == 200
    body = await resp.json()
    assert [row["log_id"] for row in body] == ["matching"]


async def test_reprocess_logs_bulk_filters_by_description_re(
    aiohttp_client, db, reprocess_logs_stub
):
    client = await create_api_client(aiohttp_client, db, user=ADMIN_USER)
    async with db.acquire() as conn:
        await _insert_codebase(conn, "foo")
        await _insert_codebase(conn, "bar")
        now = utcnow()
        await _insert_run(
            conn,
            run_id="matching",
            codebase="foo",
            start_time=now - timedelta(minutes=30),
            finish_time=now,
            result_code="build-failed",
            description="sbuild timed out",
        )
        await _insert_run(
            conn,
            run_id="non-matching",
            codebase="bar",
            start_time=now - timedelta(minutes=30),
            finish_time=now,
            result_code="build-failed",
            description="missing build dependency",
        )

    resp = await client.post("/reprocess-logs", data={"description_re": "timed out"})
    assert resp.status == 200
    body = await resp.json()
    assert [row["log_id"] for row in body] == ["matching"]


async def test_reprocess_logs_bulk_filters_by_min_age(
    aiohttp_client, db, reprocess_logs_stub
):
    client = await create_api_client(aiohttp_client, db, user=ADMIN_USER)
    async with db.acquire() as conn:
        await _insert_codebase(conn, "foo")
        await _insert_codebase(conn, "bar")
        now = utcnow()
        await _insert_run(
            conn,
            run_id="old-enough",
            codebase="foo",
            start_time=now - timedelta(days=10, minutes=30),
            finish_time=now - timedelta(days=10),
            result_code="build-failed",
        )
        await _insert_run(
            conn,
            run_id="too-recent",
            codebase="bar",
            start_time=now - timedelta(minutes=30),
            finish_time=now,
            result_code="build-failed",
        )

    resp = await client.post("/reprocess-logs", data={"min_age": "5"})
    assert resp.status == 200
    body = await resp.json()
    assert [row["log_id"] for row in body] == ["old-enough"]


async def test_reprocess_logs_bulk_invalid_regex(
    aiohttp_client, db, reprocess_logs_stub
):
    client = await create_api_client(aiohttp_client, db, user=ADMIN_USER)
    resp = await client.post("/reprocess-logs", data={"description_re": "("})
    assert resp.status == 400


async def test_reprocess_logs_bulk_selects_every_unpack_code(db):
    async with db.acquire() as conn:
        now = utcnow()
        for run_id, result_code in [
            ("patch", "unpack-patch-application-failed"),
            ("upstream", "unpack-unexpected-local-upstream-changes"),
            ("other", "success"),
        ]:
            await _insert_codebase(conn, run_id)
            await _insert_run(
                conn,
                run_id=run_id,
                codebase=run_id,
                start_time=now - timedelta(minutes=30),
                finish_time=now,
                result_code=result_code,
            )
        rows = await select_runs_to_reprocess(conn)

    assert sorted(row["id"] for row in rows) == ["patch", "upstream"]


async def _start_services(aiohttp_server):
    hang = asyncio.Event()
    hits: dict[str, int] = {}

    def json_handler(body, status=200):
        async def handler(request):
            return web.json_response(body, status=status)

        return handler

    async def text_ok(request):
        return web.Response(text="ok")

    async def hanging(request):
        await hang.wait()
        return web.Response(text="late")

    def active_runs(*workers):
        return json_handler([{"worker": w, "codebase": "foo"} for w in workers])

    unhealthy = {
        "status": "unhealthy",
        "checks": [
            {"name": "database", "status": "unhealthy"},
            {"name": "logs", "status": "healthy"},
        ],
    }
    routes = {
        "runner": [
            ("/health", json_handler({"status": "healthy", "version": "1.2.3"})),
            ("/active-runs", active_runs("carol")),
        ],
        "runner-broken": [
            ("/health", json_handler(unhealthy, status=503)),
            ("/active-runs", active_runs("carol")),
        ],
        "degraded": [("/health", json_handler({"status": "degraded"}))],
        "ok": [("/health", text_ok)],
        "hang": [("/health", hanging)],
        "oidc": [("/.well-known/openid-configuration", json_handler({"issuer": "x"}))],
        "gitlab": [("/", json_handler({"message": "401 Unauthorized"}, status=401))],
        "broken-forge": [("/", json_handler({}, status=502))],
    }
    servers = {}
    for name, handlers in routes.items():

        @web.middleware
        async def count(request, handler, name=name):
            hits[name] = hits.get(name, 0) + 1
            return await handler(request)

        app = web.Application(middlewares=[count])
        for path, handler in handlers:
            app.router.add_get(path, handler)
        servers[name] = await aiohttp_server(app)
    return servers, hang, hits


def _hostport(server):
    return f"{server.host}:{server.port}"


def _strip_volatile(status):
    for group in status["groups"]:
        for instance in group.get("instances", []):
            del instance["checked_at"]
            instance.pop("elapsed_ms", None)
            if instance["error"]:
                instance["error"] = instance["error"].split(":")[0]
    return status


async def _status_client(
    aiohttp_client,
    unused_tcp_port,
    db,
    servers,
    *,
    user,
    broken=False,
    sso=True,
    redis_down=False,
):
    closed = f"127.0.0.1:{unused_tcp_port}"
    s = {name: f"http://{_hostport(server)}/" for name, server in servers.items()}
    redis_location = f"redis://{closed}" if redis_down else "redis://127.0.0.1:6379"
    sso_config = (
        f'oauth2_provider {{ base_url: "{s["oidc"]}" admin_group: "admins" }}\n'
        if sso
        else ""
    )
    config = (
        'database_location: "postgresql://janitor:secret@127.0.0.1:5432/janitor"\n'
        f'redis_location: "{redis_location}"\n'
        f'git_location: "{s["ok"]}"\n'
        f'zipkin_address: "{f"http://{closed}/" if broken else s["gitlab"]}v1/traces"\n'
        f"{sso_config}"
        'distribution { name: "unstable" '
        f'archive_mirror_uri: "{f"http://{closed}/" if broken else s["ok"]}debian" }}\n'
    )
    if redis_down:
        redis = Redis.from_url(redis_location)
    else:
        redis = FakeRedis()
        now = int(datetime.now(timezone.utc).timestamp())
        await redis.set("worker:last_seen:alice", now)
        await redis.set("worker:last_seen:bob", now - 3600 if broken else now)
        await redis.set("worker:last_seen:carol", "not a timestamp")
    async with db.acquire() as conn:
        await conn.execute(
            "INSERT INTO worker (name, password, link) VALUES "
            "('alice', 'x', 'http://10.0.0.1:9821/'), ('bob', 'x', NULL), "
            "('carol', 'x', NULL)"
        )
        for origin in [s["gitlab"]] + ([s["broken-forge"]] if broken else []):
            await conn.execute(
                "INSERT INTO merge_proposal (url) VALUES ($1)", origin + "a/b/pull/1"
            )
    status_checks = [("auto_upload", s["ok"] + "health")]
    if broken:
        status_checks.append(("ognibuild_dep", f"http://{closed}/health"))
    return await create_client(
        aiohttp_client,
        db,
        user=user,
        config=config,
        redis=redis,
        runner_url=(
            f"http://{closed}/"
            if redis_down
            else s["runner-broken"]
            if broken
            else s["runner"]
        ),
        publisher_url=URL(s["hang"] if broken else s["ok"]),
        archiver_url=f"http://{closed}/" if broken else s["ok"],
        differ_url=s["degraded"] if broken else s["ok"],
        status_checks=status_checks,
    )


def _group(section, service, instances, state="ok", up=None):
    return {
        "section": section,
        "service": service,
        "state": state,
        "up": len(instances) if up is None else up,
        "total": len(instances),
        "instances": instances,
    }


def _instance(section, service, name, target, state="ok", **kwargs):
    return dict(
        {
            "section": section,
            "service": service,
            "name": name,
            "target": target,
            "state": state,
            "http_status": None,
            "error": None,
            "health": {},
        },
        **kwargs,
    )


STATUS_ADMIN = {"email": "admin@example.com", "groups": ["admins"]}


async def test_status_all_up(aiohttp_client, aiohttp_server, unused_tcp_port, db):
    servers, _hang, hits = await _start_services(aiohttp_server)
    client = await _status_client(
        aiohttp_client, unused_tcp_port, db, servers, user=STATUS_ADMIN, broken=False
    )
    s = {name: _hostport(server) for name, server in servers.items()}
    for _ in range(2):
        resp = await client.get(
            "/cupboard/status", headers={"Accept": "application/json"}
        )
        assert resp.status == 200
    status = _strip_volatile(await resp.json())
    seen, bob_seen, carol_seen = [
        i["last_seen"]
        for g in status["groups"]
        if g["service"] == "worker"
        for i in g["instances"]
    ]
    assert (bob_seen, carol_seen) == (seen, None)
    assert utcnow() - datetime.fromisoformat(seen).replace(tzinfo=None) < timedelta(
        minutes=1
    )

    def worker(name, target, active, last_seen):
        return _instance(
            "core", "worker", name, target, active_runs=active, last_seen=last_seen
        )

    def ok(section, service, name=s["ok"], **kwargs):
        return _instance(section, service, name, s["ok"], http_status=200, **kwargs)

    assert status == {
        "state": "ok",
        "groups": [
            _group(
                "core",
                "database",
                [_instance("core", "database", "database", "127.0.0.1:5432")],
            ),
            _group(
                "core", "redis", [_instance("core", "redis", "redis", "127.0.0.1:6379")]
            ),
            _group(
                "core",
                "runner",
                [
                    _instance(
                        "core",
                        "runner",
                        s["runner"],
                        s["runner"],
                        http_status=200,
                        health={"status": "healthy", "version": "1.2.3"},
                    )
                ],
            ),
        ]
        + [
            _group("core", service, [ok("core", service)])
            for service in ["publish", "archive", "differ", "git_store"]
        ]
        + [
            _group("core", "bzr_store", [], state="not configured"),
            _group(
                "core",
                "worker",
                [
                    worker("alice", "10.0.0.1:9821", 0, seen),
                    worker("bob", None, 0, seen),
                    worker("carol", None, 1, None),
                ],
            ),
            _group("optional", "auto_upload", [ok("optional", "auto_upload")]),
            _group(
                "optional",
                "tracing",
                [
                    _instance(
                        "optional",
                        "tracing",
                        s["gitlab"],
                        s["gitlab"],
                        http_status=404,
                    )
                ],
            ),
            _group(
                "external",
                "sso",
                [_instance("external", "sso", "127.0.0.1", s["oidc"], http_status=200)],
            ),
            _group(
                "external",
                "mirror",
                [
                    _instance(
                        "external", "mirror", "127.0.0.1", s["ok"], http_status=404
                    )
                ],
            ),
            _group(
                "external",
                "forge",
                [
                    _instance(
                        "external", "forge", s["gitlab"], s["gitlab"], http_status=401
                    )
                ],
            ),
        ],
        "active_runs": 1,
        "queue_length": 0,
    }
    # Two page loads: the internal services twice, the external ones once.
    assert hits == {"runner": 4, "ok": 11, "oidc": 1, "gitlab": 2}


async def test_status_failures(aiohttp_client, aiohttp_server, unused_tcp_port, db):
    servers, hang, _hits = await _start_services(aiohttp_server)
    client = await _status_client(
        aiohttp_client, unused_tcp_port, db, servers, user=STATUS_ADMIN, broken=True
    )
    s = {name: _hostport(server) for name, server in servers.items()}
    closed = f"127.0.0.1:{unused_tcp_port}"
    json_resp, html_resp = await asyncio.gather(
        client.get("/cupboard/status", headers={"Accept": "application/json"}),
        client.get("/cupboard/status"),
    )
    hang.set()
    assert (json_resp.status, html_resp.status) == (200, 200)
    status = _strip_volatile(await json_resp.json())
    assert status["state"] == "down"
    assert {
        (g["section"], g["service"]): (g["state"], g["up"], g["total"])
        for g in status["groups"]
    } == {
        ("core", "database"): ("ok", 1, 1),
        ("core", "redis"): ("ok", 1, 1),
        ("core", "runner"): ("down", 0, 1),
        ("core", "publish"): ("down", 0, 1),
        ("core", "archive"): ("down", 0, 1),
        ("core", "differ"): ("degraded", 0, 1),
        ("core", "git_store"): ("ok", 1, 1),
        ("core", "bzr_store"): ("not configured", 0, 0),
        ("core", "worker"): ("degraded", 2, 3),
        ("optional", "auto_upload"): ("ok", 1, 1),
        ("optional", "ognibuild_dep"): ("down", 0, 1),
        ("optional", "tracing"): ("down", 0, 1),
        ("external", "sso"): ("ok", 1, 1),
        ("external", "mirror"): ("down", 0, 1),
        ("external", "forge"): ("degraded", 1, 2),
    }
    assert {
        (g["service"], i["name"]): (
            i["state"],
            i["http_status"],
            i["error"],
            i["health"],
        )
        for g in status["groups"]
        for i in g["instances"]
        if g["state"] != "ok"
    } == {
        ("runner", s["runner-broken"]): (
            "down",
            503,
            "HTTP 503",
            {"status": "unhealthy", "checks": "database: unhealthy, logs: healthy"},
        ),
        ("publish", s["hang"]): ("down", None, "timed out after 2 s", {}),
        ("archive", closed): ("down", None, "unreachable", {}),
        ("differ", s["degraded"]): ("degraded", 200, None, {"status": "degraded"}),
        ("worker", "alice"): ("ok", None, None, {}),
        ("worker", "bob"): ("not seen", None, None, {}),
        ("worker", "carol"): ("ok", None, None, {}),
        ("ognibuild_dep", closed): ("down", None, "unreachable", {}),
        ("tracing", closed): ("down", None, "unreachable", {}),
        ("mirror", "127.0.0.1"): ("down", None, "unreachable", {}),
        ("forge", s["gitlab"]): ("ok", 401, None, {}),
        ("forge", s["broken-forge"]): ("down", 502, "HTTP 502", {}),
    }

    soup = BeautifulSoup(await html_resp.text(), "html.parser")
    assert {
        table["id"]: [
            tr.select("td")[1].get_text(" ", strip=True)
            for tr in table.select("tr.service")
        ]
        for table in soup.find_all("table")
    } == {
        "status-core": [
            "ok",
            "ok",
            "down",
            "down",
            "down",
            "degraded",
            "ok",
            "not configured",
            "degraded (2 of 3 checks ok)",
        ],
        "status-optional": ["ok", "down", "down"],
        "status-external": ["ok", "down", "degraded (1 of 2 checks ok)"],
    }
    assert [e.get_text(" ", strip=True) for e in soup.select("#overall-status")] == [
        "Overall status: down"
    ]


async def test_status_redis_and_runner_down(
    aiohttp_client, aiohttp_server, unused_tcp_port, db
):
    servers, _hang, _hits = await _start_services(aiohttp_server)
    client = await _status_client(
        aiohttp_client, unused_tcp_port, db, servers, user=STATUS_ADMIN, redis_down=True
    )
    resp = await client.get("/cupboard/status", headers={"Accept": "application/json"})
    status = _strip_volatile(await resp.json())
    groups = {g["service"]: g for g in status["groups"]}
    assert (resp.status, status["state"], status["active_runs"]) == (200, "down", None)
    assert [
        (g["state"], [i["error"] for i in g["instances"]])
        for g in (groups["redis"], groups["runner"])
    ] == [("down", ["ConnectionError"]), ("down", ["unreachable"])]
    # Without Redis the last-seen time is unknown, so it is left out.
    assert (groups["worker"]["state"], groups["worker"]["instances"]) == (
        "unknown",
        [
            _instance("core", "worker", name, target, state="unknown", active_runs=None)
            for name, target in [
                ("alice", "10.0.0.1:9821"),
                ("bob", None),
                ("carol", None),
            ]
        ],
    )


@pytest.mark.parametrize(
    "workers, group",
    [([], ("not configured", 0, 0)), (["alice", "bob"], ("degraded", 0, 2))],
)
async def test_status_workers_not_seen(
    aiohttp_client, aiohttp_server, unused_tcp_port, db, workers, group
):
    servers, _hang, _hits = await _start_services(aiohttp_server)
    client = await _status_client(
        aiohttp_client, unused_tcp_port, db, servers, user=STATUS_ADMIN
    )
    async with db.acquire() as conn:
        await conn.execute(
            "DELETE FROM worker WHERE NOT (name = ANY($1::text[]))", workers
        )
    await client.app["redis"].delete("worker:last_seen:alice", "worker:last_seen:bob")
    resp = await client.get("/cupboard/status", headers={"Accept": "application/json"})
    status = await resp.json()
    [worker] = [g for g in status["groups"] if g["service"] == "worker"]
    assert (
        (worker["state"], worker["up"], worker["total"]),
        [(i["name"], i["state"], i["last_seen"]) for i in worker["instances"]],
        status["state"],
    ) == (group, [(name, "not seen", None) for name in workers], "degraded")


@pytest.mark.parametrize(
    "user", [None, {"email": "user@example.com", "groups": []}, STATUS_ADMIN]
)
async def test_status_page_details_only_for_admins(
    aiohttp_client, aiohttp_server, unused_tcp_port, db, user
):
    servers, _hang, _hits = await _start_services(aiohttp_server)
    client = await _status_client(
        aiohttp_client,
        unused_tcp_port,
        db,
        servers,
        user=user,
        broken=False,
        sso=user is not None,
    )
    admin = user is STATUS_ADMIN
    resp = await client.get("/cupboard/status", headers={"Accept": "application/json"})
    status = await resp.json()
    assert (resp.status, sorted(status)) == (
        200,
        ["active_runs", "groups", "queue_length", "state"]
        if admin
        else ["groups", "state"],
    )
    assert [sorted(g) for g in status["groups"]] == [
        ["instances", "section", "service", "state", "total", "up"]
        if admin
        else ["section", "service", "state", "total", "up"]
    ] * (13 if user is None else 14)

    resp = await client.get("/cupboard/status")
    assert (resp.status, resp.content_type) == (200, "text/html")
    html = await resp.text()
    soup = BeautifulSoup(html, "html.parser")
    tables = soup.find_all("table")
    assert [t["id"] for t in tables] == [
        "status-core",
        "status-optional",
        "status-external",
    ]
    assert [th.get_text() for th in tables[0].find_all("th")] == (
        [
            "Service",
            "State",
            "Address",
            "HTTP status",
            "Response time",
            "Error",
            "Details",
            "Last checked",
        ]
        if admin
        else ["Service", "State"]
    )
    assert [
        [td.get_text(" ", strip=True) for td in tr.find_all("td")]
        for tr in tables[0].select("tr.service")
    ] == [
        [service, state] + ([""] if admin else [])
        for service, state in [
            ("database", "ok"),
            ("redis", "ok"),
            ("runner", "ok"),
            ("publish", "ok"),
            ("archive", "ok"),
            ("differ", "ok"),
            ("git_store", "ok"),
            ("bzr_store", "not configured"),
            ("worker", "ok"),
        ]
    ]
    assert len(soup.select("tr.instance")) == (15 if admin else 0)
    addresses = [_hostport(servers[n]) for n in ["runner", "ok", "oidc", "gitlab"]]
    addresses += ["127.0.0.1:5432", "10.0.0.1:9821"]
    assert [a for a in addresses if a in html] == (addresses if admin else [])
    assert (soup.find(id="queue-status") is not None) == admin
