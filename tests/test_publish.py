#!/usr/bin/python
# Copyright (C) 2026 Jelmer Vernooij <jelmer@jelmer.uk>
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

import pytest

import janitor.publish as publish


async def test_credentials_missing_ssh_dir_returns_no_keys(aiohttp_client, monkeypatch):
    from aiohttp import web

    app = web.Application()
    app.router.add_routes(publish.routes)
    app["gpg"] = type("FakeGpg", (), {"keylist": lambda self, secret=False: []})()

    monkeypatch.setattr(publish, "forges", {})
    monkeypatch.setattr(publish.os.path, "expanduser", lambda p: "/nonexistent-ssh-dir-for-test")

    client = await aiohttp_client(app)
    resp = await client.get("/credentials")
    assert resp.status == 200
    body = await resp.json()
    assert body["ssh_keys"] == []


class _FakeVcsManager:
    def get_branch_url(self, codebase, branch_name):
        return f"https://example.com/{codebase}/{branch_name}"


async def test_publish_one_sends_revision_id_and_invokes_compiled_binary(monkeypatch):
    captured = {}

    async def fake_run_worker_process(args, request, **kwargs):
        captured["args"] = args
        captured["request"] = request
        return 1, {"code": "some-failure", "description": "boom"}

    monkeypatch.setattr(publish, "run_worker_process", fake_run_worker_process)

    worker = publish.PublishWorker()

    with pytest.raises(publish.PublishFailure):
        await worker.publish_one(
            campaign="lintian-fixes",
            codebase="mypkg",
            command="lintian-brush",
            target_branch_url="https://example.com/mypkg",
            mode="propose",
            role="main",
            revision=b"somerevid",
            log_id="log-1",
            unchanged_id=None,
            derived_branch_name="lintian-fixes",
            rate_limit_bucket=None,
            vcs_manager=_FakeVcsManager(),
        )

    assert captured["args"] == ["janitor-publish-one"]
    assert captured["request"]["revision_id"] == "somerevid"
    assert "revision" not in captured["request"]


async def test_publish_one_passes_template_env_path_to_compiled_binary(monkeypatch):
    captured = {}

    async def fake_run_worker_process(args, request, **kwargs):
        captured["args"] = args
        return 1, {"code": "some-failure", "description": "boom"}

    monkeypatch.setattr(publish, "run_worker_process", fake_run_worker_process)

    worker = publish.PublishWorker(template_env_path="/etc/janitor/templates")

    with pytest.raises(publish.PublishFailure):
        await worker.publish_one(
            campaign="lintian-fixes",
            codebase="mypkg",
            command="lintian-brush",
            target_branch_url="https://example.com/mypkg",
            mode="propose",
            role="main",
            revision=b"somerevid",
            log_id="log-1",
            unchanged_id=None,
            derived_branch_name="lintian-fixes",
            rate_limit_bucket=None,
            vcs_manager=_FakeVcsManager(),
        )

    assert captured["args"] == [
        "janitor-publish-one",
        "--template-env-path=/etc/janitor/templates",
    ]


class _FakeAcquireCM:
    def __init__(self, conn):
        self._conn = conn

    async def __aenter__(self):
        return self._conn

    async def __aexit__(self, *exc_info):
        return False


class _FakeDb:
    def __init__(self, conn=None):
        self._conn = conn

    def acquire(self):
        return _FakeAcquireCM(self._conn)


async def test_publish_request_without_policy_returns_graceful_error(
    aiohttp_client, monkeypatch
):
    from aiohttp import web

    class _FakeRun:
        id = "run-1"
        result_branches = [("main", None, None, None)]

    async def fake_get_last_effective_run(conn, codebase, campaign):
        return _FakeRun()

    async def fake_get_publish_policy(conn, codebase, campaign):
        return None, None, None

    monkeypatch.setattr(publish, "get_last_effective_run", fake_get_last_effective_run)
    monkeypatch.setattr(publish, "get_publish_policy", fake_get_publish_policy)

    app = web.Application()
    app.router.add_routes(publish.routes)
    app["db"] = _FakeDb()
    app["vcs_managers"] = {}
    app["bucket_rate_limiter"] = None

    client = await aiohttp_client(app)
    resp = await client.post("/lintian-fixes/mypkg/publish")
    assert resp.status == 400
    body = await resp.json()
    assert body["code"] == "missing-publish-policy"
    assert body["run_id"] == "run-1"


async def test_get_publish_policy_returns_no_policy_for_unpopulated_row():
    fake_row = {"per_branch_policy": None, "command": None, "rate_limit_bucket": None}

    class _FakeConn:
        async def fetchrow(self, query, *args):
            return fake_row

    result = await publish.get_publish_policy(_FakeConn(), "mycodebase", "lintian-fixes")
    assert result == (None, None, None)


async def test_handle_policy_get_returns_per_branch_policy(aiohttp_client, monkeypatch):
    from aiohttp import web

    fake_row = {
        "rate_limit_bucket": "default",
        "per_branch_policy": [
            {"role": "main", "mode": "propose", "frequency_days": 3},
        ],
    }

    class _FakeConn:
        async def fetchrow(self, query, *args):
            return fake_row

    app = web.Application()
    app.router.add_routes(publish.routes)
    app["db"] = _FakeDb(_FakeConn())

    client = await aiohttp_client(app)
    resp = await client.get("/policy/default")
    assert resp.status == 200
    body = await resp.json()
    assert body["rate_limit_bucket"] == "default"
    assert body["per_branch"]["main"] == {"mode": "propose", "max_frequency_days": 3}
