# pagesift crawler identity

The default HTTP User-Agent identifies the running pagesift package version and links to this page. The engine respects robots.txt by default, applies per-site rate limits and backs off after rate-limit responses. Operators may configure several egress proxies, so requests can come from different IP addresses; the per-site rate limit applies to all of them combined. When one egress is blocked by IP reputation or shown a challenge page, pagesift may retry once through a different egress, and reports the block if it persists. It does not impersonate browsers, change its User-Agent to evade blocks, or solve CAPTCHAs.

Site owners can use a `User-agent: pagesift` robots.txt group to specify crawling rules. Instance operators choose the URLs, schedules and contact details; this open-source project does not operate every pagesift deployment. Configure an honest instance-specific User-Agent and contact URL when running a public crawler.

For abuse from a specific deployment, contact that deployment's operator or hosting provider. For engine vulnerabilities use the repository's [security reporting instructions](../SECURITY.md).
