"""1.5 Loop sync source lifecycle & DataGraph query tests (sync & async)."""

from __future__ import annotations

import asyncio

import pytest

import pagesift


def test_sync_lifecycle_and_graph_queries(server, tmp_path):
    with pagesift.Scraper(
        allow_private_network=True,
        request_interval_ms=0,
        per_host_concurrency=8,
        store_path=tmp_path,
    ) as s:
        created = s.sync(
            action="create",
            source_id="py-site",
            seeds=[f"{server}/site/"],
            max_depth=1,
            max_requests_per_tick=5,
            allow_browser=False,
        )
        assert created["success"] is True
        assert created["source"]["id"] == "py-site"

        listed = s.sync({"action": "list"})
        assert listed["total"] == 1

        tick = s.sync(action="run", id="py-site", max_requests=3)
        assert tick["success"] is True
        assert tick["delta"]["sourceId"] == "py-site"
        assert tick["delta"]["stats"]["requestsExecuted"] >= 1

        state = s.sync(action="state", id="py-site")
        assert state["success"] is True
        assert state["state"]["spec"]["id"] == "py-site"

        deltas = s.sync(action="deltas", id="py-site", limit=5)
        assert deltas["success"] is True
        assert deltas["total"] == 1

        hist = s.query_graph(history_url=f"{server}/site/")
        assert hist["success"] is True
        assert len(hist["versions"]) >= 1

        claims = s.query_graph({"claims": True, "url": f"{server}/site/"})
        assert claims["success"] is True
        assert isinstance(claims["total"], int)
        assert isinstance(claims.get("claims", []), list)

        deleted = s.sync(action="delete", id="py-site")
        assert deleted["deleted"] is True

        with pytest.raises(pagesift.PagesiftError) as exc:
            s.sync(action="get", id="py-site")
        assert exc.value.code == "NOT_FOUND"


def test_async_sync_and_aquery_graph(server, tmp_path):
    async def main():
        async with pagesift.Scraper(
            allow_private_network=True,
            request_interval_ms=0,
            per_host_concurrency=8,
            store_path=tmp_path,
        ) as s:
            await s.async_sync(
                {
                    "action": "create",
                    "id": "py-async",
                    "seeds": [f"{server}/site/"],
                    "maxDepth": 1,
                    "maxRequestsPerTick": 3,
                    "allowBrowser": False,
                }
            )
            tick = await s.async_sync(action="run", id="py-async")
            assert tick["success"] is True
            hist = await s.aquery_graph(history_url=f"{server}/site/")
            assert len(hist["versions"]) >= 1

    asyncio.run(main())


def test_loop_requires_store_path(scraper):
    with pytest.raises(pagesift.InvalidRequest):
        scraper.sync(action="list")
    with pytest.raises(pagesift.InvalidRequest):
        scraper.query_graph(conflicts=True)
