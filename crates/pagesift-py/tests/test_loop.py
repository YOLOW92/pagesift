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
        assert isinstance(created["source"]["instanceId"], str)
        assert created["source"]["instanceId"]

        pre_create_state = s.sync(
            action="state",
            id="py-site",
            as_of=created["source"]["createdAtMs"] - 1,
        )
        assert pre_create_state["state"]["historyStatus"] == "not_created_yet"
        assert pre_create_state["state"].get("spec") is None

        listed = s.sync({"action": "list"})
        assert listed["total"] == 1

        tick = s.sync(action="run", id="py-site", max_requests=3)
        assert tick["success"] is True
        assert tick["delta"]["sourceId"] == "py-site"
        assert tick["delta"]["instanceId"] == created["source"]["instanceId"]
        assert tick["delta"]["stats"]["requestsExecuted"] >= 1

        state = s.sync(action="state", id="py-site")
        assert state["success"] is True
        assert state["state"]["spec"]["id"] == "py-site"
        assert state["state"]["spec"]["instanceId"] == created["source"]["instanceId"]

        deltas = s.sync(action="deltas", id="py-site", limit=5)
        assert deltas["success"] is True
        assert deltas["total"] == 1

        hist = s.query_graph(history_url=f"{server}/site/")
        assert hist["success"] is True
        assert len(hist["versions"]) >= 1

        claims = s.query_graph({"claims": True, "url": f"{server}/site/"})
        assert claims["success"] is True
        assert isinstance(claims["total"], int)
        assert claims.get("historyUnavailable", False) is False
        assert isinstance(claims.get("claims", []), list)

        deleted = s.sync(action="delete", id="py-site")
        assert deleted["deleted"] is True

        with pytest.raises(pagesift.PagesiftError) as exc:
            s.sync(action="get", id="py-site")
        assert exc.value.code == "NOT_FOUND"

        recreated = s.sync(
            action="create",
            source_id="py-site",
            seeds=[f"{server}/site/"],
            max_depth=1,
            max_requests_per_tick=5,
            allow_browser=False,
        )
        assert recreated["success"] is True
        assert recreated["source"]["instanceId"] != created["source"]["instanceId"]
        recreated_deltas = s.sync(action="deltas", id="py-site", limit=5)
        assert recreated_deltas["total"] == 0


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


def test_context_inline_and_persistent_lifecycle(tmp_path):
    with pagesift.Scraper() as ephemeral:
        inline = ephemeral.context(
            action="pack",
            query="API Quotas Pro RPM",
            budget_tokens=256,
            documents=[
                {
                    "url": "https://docs.example.com/limits",
                    "markdown": "# API Quotas\n\n| Tier | RPM |\n|---|---|\n| Pro | 600 |\n",
                }
            ],
        )
        assert inline["success"] is True
        assert len(inline["pack"]["provenanceManifest"]) >= 1
        assert "Tier" in inline["pack"]["compiledContext"]

    with pagesift.Scraper(store_path=tmp_path) as s:
        ingested = s.context(
            action="ingest",
            source_id="docs",
            documents=[
                {
                    "url": "https://docs.example.com/pricing",
                    "markdown": "# Enterprise Plan\n\nPrice: $49 per seat unless annual billing applies.\n",
                }
            ],
        )
        assert ingested["success"] is True
        assert ingested["liveUnitCount"] >= 1
        instance_id = ingested["delta"]["instanceId"]

        searched = s.context(action="search", query="Enterprise seat price", source_id="docs")
        assert searched["success"] is True
        assert len(searched["report"]["hits"]) >= 1

        polled = s.context(action="deltas", source_id="docs", consumer_id="worker-1")
        assert polled["success"] is True
        assert len(polled["poll"]["deltas"]) == 1

        acked = s.context(
            action="ack",
            source_id="docs",
            consumer_id="worker-1",
            instance_id=instance_id,
            acked_delta_seq=1,
        )
        assert acked["success"] is True
        assert acked["checkpoint"]["ackedDeltaSeq"] == 1

    del s
    import gc

    gc.collect()

    async def verify_async():
        async with pagesift.Scraper(store_path=tmp_path) as s:
            act = await s.acontext(
                action="action",
                mode="context",
                query="Enterprise seat price",
                source_ids=["docs"],
            )
            assert act["success"] is True
            assert act["outcome"]["reusedExistingContext"] is True

    asyncio.run(verify_async())
