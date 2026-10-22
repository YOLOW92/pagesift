# pagesift crawler identity

The default HTTP User-Agent identifies the running pagesift package version and links to this page. The engine respects robots.txt, applies per-host rate limits and backs off after rate-limit responses. It does not impersonate browsers or bypass access barriers.

Site owners can use a `User-agent: pagesift` robots.txt group to specify crawling rules. Instance operators choose the URLs, schedules and contact details; this open-source project does not operate every pagesift deployment. Configure an honest instance-specific User-Agent and contact URL when running a public crawler.

For abuse from a specific deployment, contact that deployment's operator or hosting provider. For engine vulnerabilities use the repository's [security reporting instructions](../SECURITY.md).
