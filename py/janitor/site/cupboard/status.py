"""Health of the services behind this site, for /cupboard/status."""

import asyncio
import re
import time
from datetime import datetime, timedelta, timezone

import asyncpg
from aiohttp import ClientError, ClientTimeout
from yarl import URL

from janitor import utcnow
from redis.exceptions import RedisError

TIMEOUT = 2.0
EXTERNAL_CACHE_SECONDS = 60
WORKER_SEEN = timedelta(minutes=5)

SECTIONS = ["core", "optional", "external"]
CORE_SERVICES = [
    "database",
    "redis",
    "runner",
    "publish",
    "archive",
    "differ",
    "git_store",
    "bzr_store",
    "worker",
]

DB_ERRORS = (
    asyncio.TimeoutError,
    OSError,
    asyncpg.PostgresError,
    asyncpg.InterfaceError,
)


def _target(url):
    try:
        url = URL(str(url))
        return f"{url.host}:{url.port}" if url.port else url.host
    except ValueError:
        return None


def _clean_error(text):
    return re.sub(r"//[^/@\s]*@", "//***@", text)[:200]


def _health_fields(body):
    if not isinstance(body, dict):
        return {}
    fields = {k: body[k] for k in ["status", "version"] if isinstance(body.get(k), str)}
    checks = body.get("checks")
    if isinstance(checks, list):
        fields["checks"] = ", ".join(
            f"{c.get('name')}: {c.get('status')}" for c in checks if isinstance(c, dict)
        )
    return fields


def _result(target, **kwargs):
    return dict(
        {
            "target": target,
            "state": "down",
            "http_status": None,
            "error": None,
            "health": {},
            "elapsed_ms": None,
            "checked_at": utcnow(),
        },
        **kwargs,
    )


async def probe_http(session, url, *, reachable=False):
    # Third-party hosts are up for any reply below 500, e.g. GitLab 401.
    result = _result(_target(url))
    start = time.monotonic()
    try:
        async with session.get(
            URL(str(url)).with_user(None),
            timeout=ClientTimeout(total=TIMEOUT),
            allow_redirects=False,
        ) as resp:
            result["http_status"] = resp.status
            if reachable:
                result["state"] = "ok" if resp.status < 500 else "down"
            else:
                if resp.content_type == "application/json":
                    try:
                        result["health"] = _health_fields(await resp.json())
                    except ValueError:
                        pass
                if 200 <= resp.status < 300:
                    degraded = result["health"].get("status") == "degraded"
                    result["state"] = "degraded" if degraded else "ok"
            if result["state"] == "down":
                result["error"] = f"HTTP {resp.status}"
    except asyncio.TimeoutError:
        result["error"] = f"timed out after {TIMEOUT:g} s"
    except (ClientError, ValueError) as e:
        result["error"] = _clean_error(f"unreachable: {e}")
    result["elapsed_ms"] = round((time.monotonic() - start) * 1000)
    return result


async def _probe_call(target, call):
    result = _result(target)
    start = time.monotonic()
    try:
        await asyncio.wait_for(call(), TIMEOUT)
        result["state"] = "ok"
    except asyncio.TimeoutError:
        result["error"] = f"timed out after {TIMEOUT:g} s"
    except (*DB_ERRORS, RedisError) as e:
        result["error"] = _clean_error(f"{type(e).__name__}: {e}")
    result["elapsed_ms"] = round((time.monotonic() - start) * 1000)
    return result


async def _cached(app, key, probe):
    cache = app["status_cache"]
    hit = cache.get(key)
    if hit is not None and time.monotonic() - hit[0] < EXTERNAL_CACHE_SECONDS:
        return hit[1]
    result = await probe()
    cache[key] = (time.monotonic(), result)
    return result


async def _instance(section, service, name, probe):
    return [dict(await probe, section=section, service=service, name=name)]


def _unknown(section, service, what):
    return [
        _result(
            None,
            section=section,
            service=service,
            name=what,
            state="unknown",
            error="database unavailable",
        )
    ]


async def _fetch(pool, query):
    try:
        return await asyncio.wait_for(pool.fetch(query), TIMEOUT)
    except DB_ERRORS:
        return None


async def _forges(app, session):
    rows = await _fetch(
        app["pool"],
        "SELECT DISTINCT substring(url from '^https?://[^/]+') AS origin "
        "FROM merge_proposal ORDER BY origin",
    )
    if rows is None:
        return _unknown("external", "forge", "forge list")
    probes = []
    for row in rows:
        if not row["origin"]:
            continue
        url = row["origin"] + "/"
        probes.append(
            _instance(
                "external",
                "forge",
                row["origin"].split("//", 1)[1],
                _cached(
                    app, url, lambda url=url: probe_http(session, url, reachable=True)
                ),
            )
        )
    return [i for group in await asyncio.gather(*probes) for i in group]


def _timestamp(value):
    try:
        return datetime.fromtimestamp(int(value), timezone.utc).replace(tzinfo=None)
    except (TypeError, ValueError):
        return None


async def _workers(app, active_runs_task):
    workers = await _fetch(app["pool"], "SELECT name, link FROM worker ORDER BY name")
    if workers is None:
        return _unknown("core", "worker", "worker list")
    last_seen = None
    if workers and app.get("redis") is not None:
        try:
            last_seen = await asyncio.wait_for(
                app["redis"].mget([f"worker:last_seen:{w['name']}" for w in workers]),
                TIMEOUT,
            )
        except (asyncio.TimeoutError, OSError, RedisError):
            pass
    active_runs = await active_runs_task
    now = utcnow()
    instances = []
    for i, worker in enumerate(workers):
        seen = _timestamp(last_seen[i]) if last_seen else None
        active = None
        if active_runs is not None:
            active = sum(
                1 for run in active_runs if run.get("worker") == worker["name"]
            )
        if active or (seen is not None and now - seen < WORKER_SEEN):
            state = "ok"
        elif last_seen is None or active_runs is None:
            state = "unknown"
        else:
            state = "not seen"
        instances.append(
            _result(
                _target(worker["link"]) if worker["link"] else None,
                section="core",
                service="worker",
                name=worker["name"],
                state=state,
                active_runs=active,
                checked_at=now,
                # Left out when Redis could not be read, so it is not read as "never".
                **({} if last_seen is None else {"last_seen": seen}),
            )
        )
    return instances


async def _active_runs(session, runner_url):
    if not runner_url:
        return None
    try:
        async with session.get(
            URL(str(runner_url)) / "active-runs", timeout=ClientTimeout(total=TIMEOUT)
        ) as resp:
            runs = await resp.json() if resp.status == 200 else None
    except (asyncio.TimeoutError, ClientError, ValueError):
        return None
    return [r for r in runs if isinstance(r, dict)] if isinstance(runs, list) else None


def _aggregate(instances):
    up = sum(1 for i in instances if i["state"] == "ok")
    states = {i["state"] for i in instances}
    if not instances:
        state = "not configured"
    elif up == len(instances):
        state = "ok"
    elif up or "degraded" in states:
        state = "degraded"
    elif "down" in states:
        state = "down"
    # A worker that is "not seen" may still be fine, so it never makes a group down.
    elif "not seen" in states:
        state = "degraded"
    else:
        state = "unknown"
    return {"state": state, "up": up, "total": len(instances)}


def _overall(groups):
    configured = [
        g for g in groups if g["state"] != "not configured" or g["service"] == "worker"
    ]
    if any(g["section"] == "core" and g["state"] == "down" for g in configured):
        return "down"
    if any(g["state"] != "ok" for g in configured):
        return "degraded"
    return "ok"


def _is_http(url):
    try:
        return bool(url) and URL(str(url)).scheme in ("http", "https")
    except ValueError:
        return False


async def gather_status(app, *, admin=False):
    """Check every configured service at once; only admins get instance details."""
    session = app["http_client_session"]
    config = app["config"]
    pool = app["pool"]
    checks = []

    def http(section, service, url, reachable=False, cached=False):
        def probe():
            return probe_http(session, url, reachable=reachable)

        name = URL(str(url)).host if section == "external" else _target(url)
        probed = _cached(app, str(url), probe) if cached else probe()
        checks.append(_instance(section, service, name, probed))

    if config.database_location:
        checks.append(
            _instance(
                "core",
                "database",
                "database",
                _probe_call(
                    _target(config.database_location),
                    lambda: pool.fetchval("SELECT 1"),
                ),
            )
        )
    if app.get("redis") is not None and config.redis_location:
        checks.append(
            _instance(
                "core",
                "redis",
                "redis",
                _probe_call(_target(config.redis_location), app["redis"].ping),
            )
        )
    for service, url in [
        ("runner", app.get("runner_url")),
        ("publish", app.get("publisher_url")),
        ("archive", app.get("archiver_url")),
        ("differ", app.get("differ_url")),
        ("git_store", config.git_location),
        ("bzr_store", config.bzr_location),
    ]:
        if _is_http(url):
            http("core", service, URL(str(url)) / "health")
    for service, url in app.get("status_checks", []):
        http("core" if service in CORE_SERVICES else "optional", service, url)
    for service, url in [
        ("logs", config.logs_location),
        ("tracing", config.zipkin_address),
    ]:
        if _is_http(url):
            http("optional", service, url, reachable=True, cached=True)

    if config.oauth2_provider and _is_http(config.oauth2_provider.base_url):
        url = URL(config.oauth2_provider.base_url).join(
            URL("/.well-known/openid-configuration")
        )
        http("external", "sso", url, cached=True)
    for mirror in sorted({d.archive_mirror_uri for d in config.distribution}):
        if _is_http(mirror):
            http("external", "mirror", mirror, reachable=True, cached=True)
    checks.append(_forges(app, session))

    active_runs = asyncio.ensure_future(_active_runs(session, app.get("runner_url")))
    checks.append(_workers(app, active_runs))
    queue = _fetch(pool, "SELECT count(*) FROM queue") if admin else asyncio.sleep(0)
    results = await asyncio.gather(queue, *checks)

    groups: dict[tuple[str, str], list[dict]] = {
        ("core", service): [] for service in CORE_SERVICES
    }
    for instance in [i for group in results[1:] for i in group]:
        groups.setdefault((instance["section"], instance["service"]), []).append(
            instance
        )
    ret: dict = {
        "groups": [
            dict(
                {"section": section, "service": service},
                **_aggregate(members),
                **({"instances": members} if admin else {}),
            )
            for (section, service), members in sorted(
                groups.items(), key=lambda g: SECTIONS.index(g[0][0])
            )
        ]
    }
    ret["state"] = _overall(ret["groups"])
    if admin:
        runs = await active_runs
        ret["active_runs"] = None if runs is None else len(runs)
        ret["queue_length"] = results[0][0][0] if results[0] else None
    else:
        await active_runs
    return ret
